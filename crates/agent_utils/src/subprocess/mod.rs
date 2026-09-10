//! Shared child-process management for agent CLI subprocesses: kill-on-close
//! Job Object containment and the newline-delimited-JSON process shape used
//! by the chat sessions.

use std::io::{BufReader, Write as _};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use nmt_platform::process::{KillOnCloseJob, decode_child_output};
use parking_lot::Mutex;
use serde_json::{Value, json};
use tracing::warn;

mod input;
mod output;
use crate::message_memory::OUTPUT_FAILURE_METHOD;
use crate::subprocess::input::InputQueue;
pub(crate) use crate::subprocess::input::{InputClass, InputError, InputTicket};
use crate::subprocess::output::{MAX_STDERR_CHUNK, read_messages, read_piece};

/// A spawned agent CLI with piped stdio, kill-on-close containment, and
/// newline-delimited JSON output. Stdout lines that parse as JSON are handed
/// to `deliver`, stderr lines to `on_stderr`, each from its own reader thread.
/// Dropping `deliver` signals EOF: the closure is owned by the reader thread
/// and dropped when the pipe closes.
pub(crate) struct JsonLineProcess {
    child: Child,
    /// Held until the root exits or forced shutdown terminates any remaining
    /// descendants.
    job: Arc<Mutex<Option<KillOnCloseJob>>>,
    stdin: Option<InputQueue>,
    /// Provider display name ("Codex", "Claude") for lifecycle error messages.
    provider: &'static str,
}

impl JsonLineProcess {
    /// Spawn `command` with all three stdio streams piped and start the two
    /// reader threads. `display_command` is the human-readable command line
    /// quoted in the spawn error.
    #[cfg(test)]
    pub(crate) fn spawn(
        command: Command,
        display_command: &str,
        provider: &'static str,
        deliver: impl Fn(Value) + Send + 'static,
        on_stderr: impl Fn(String) + Send + 'static,
    ) -> Result<Self, String> {
        Self::spawn_with_stdout_closed(
            command,
            display_command,
            provider,
            deliver,
            on_stderr,
            || {},
        )
    }

    /// Spawn a JSON-line process and report when its stdout reader reaches EOF.
    /// Shared hosts need an explicit exit signal because their router outlives
    /// every individual delivery callback.
    pub(crate) fn spawn_with_stdout_closed(
        mut command: Command,
        display_command: &str,
        provider: &'static str,
        deliver: impl Fn(Value) + Send + 'static,
        on_stderr: impl Fn(String) + Send + 'static,
        on_stdout_closed: impl FnOnce() + Send + 'static,
    ) -> Result<Self, String> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = command
            .spawn()
            .map_err(|err| format!("could not run `{display_command}`: {err}"))?;
        let job = KillOnCloseJob::attach_or_kill(&mut child).map_err(|error| error.to_string())?;

        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| format!("{provider} stdin unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| format!("{provider} stdout unavailable"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| format!("{provider} stderr unavailable"))?;

        let job = Arc::new(Mutex::new(Some(job)));
        let writer_job = Arc::clone(&job);
        let (input_tx, input_rx) = InputQueue::new();
        thread::Builder::new()
            .name(format!("{provider}-stdin"))
            .spawn(move || {
                for input in input_rx {
                    let result = input.messages.iter().try_for_each(|message| {
                        writeln!(stdin, "{message}").and_then(|_| stdin.flush())
                    });
                    if let Err(error) = result {
                        warn!(provider, %error, "agent input writer stopped");
                        // A child can close stdin without closing stdout. Terminating
                        // its tree makes the existing EOF notification reliable.
                        writer_job.lock().take();
                        break;
                    }
                }
            })
            .map_err(|error| format!("could not start {provider} input writer: {error}"))?;

        let reader_job = Arc::clone(&job);
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            if let Err(message) = read_messages(&mut reader, provider, &deliver) {
                warn!(provider, reason = %message, "agent protocol reader stopped");
                deliver(json!({"method": OUTPUT_FAILURE_METHOD, "params": {"message": message}}));
                reader_job.lock().take();
            }
            on_stdout_closed();
        });

        thread::spawn(move || {
            let mut reader = BufReader::new(stderr);
            while let Ok(line) = read_piece(&mut reader, MAX_STDERR_CHUNK) {
                if line.is_empty() {
                    break;
                }
                let line = line.strip_suffix(b"\n").unwrap_or(&line);
                on_stderr(decode_child_output(
                    line.strip_suffix(b"\r").unwrap_or(line),
                ));
            }
        });

        Ok(Self {
            child,
            job,
            stdin: Some(input_tx),
            provider,
        })
    }

    /// Queue a required control line using reserved capacity. Failure ends the
    /// process tree so a missing reply cannot leave the session waiting forever.
    pub(crate) fn write_line(&mut self, message: Value) -> Result<(), InputError> {
        let result = self.try_write_line(message, InputClass::Control);
        if let Err(error) = &result {
            warn!(%error, "required agent control input was rejected");
            // Callers use this path for required replies and lifecycle controls.
            // Exhausting even the control reserve cannot silently lose them.
            self.stdin.take();
            self.job.lock().take();
        }
        result
    }

    /// Success means admission, not completed I/O. Capacity rejection leaves
    /// the transport open so the caller can retain its input and retry.
    pub(crate) fn try_write_line(
        &mut self,
        message: Value,
        class: InputClass,
    ) -> Result<(), InputError> {
        self.try_write_batch(vec![message], class)
    }

    /// Reserve all settings and prompt lines together before publishing any of
    /// them; the caller changes its local settings only after admission.
    pub(crate) fn try_write_batch(
        &mut self,
        messages: Vec<Value>,
        class: InputClass,
    ) -> Result<(), InputError> {
        self.write_tracked(messages, class).map(|_| ())
    }

    pub(crate) fn write_tracked(
        &mut self,
        messages: Vec<Value>,
        class: InputClass,
    ) -> Result<InputTicket, InputError> {
        if !self.has_stdin() {
            return Err(InputError::Closed);
        }
        let result = self
            .stdin
            .as_ref()
            .ok_or(InputError::Closed)?
            .submit_tracked(messages, class);
        if matches!(result, Err(InputError::Closed)) {
            self.stdin.take();
            self.job.lock().take();
        }
        result
    }

    pub(crate) fn abort(&mut self) {
        self.stdin.take();
        self.job.lock().take();
    }

    /// False once shutdown has closed the protocol input.
    pub(crate) fn has_stdin(&self) -> bool {
        self.stdin.is_some() && self.job.lock().is_some()
    }

    /// Close the protocol input (EOF is the CLIs' graceful-shutdown signal)
    /// and wait for the process to exit. Forced termination is opt-in because
    /// it can interrupt an active tool operation; dropping the Job Object
    /// affects only this process's tree.
    pub(crate) fn shutdown(&mut self, timeout: Duration, force: bool) -> Result<(), String> {
        drop(self.stdin.take());
        let started = Instant::now();
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => {
                    self.job.lock().take();
                    return Ok(());
                }
                Ok(None) if started.elapsed() < timeout => {
                    thread::sleep(Duration::from_millis(20));
                }
                Ok(None) if force => {
                    self.job.lock().take();
                    self.child.wait().map_err(|error| {
                        format!("could not wait for {} to stop: {error}", self.provider)
                    })?;
                    return Ok(());
                }
                Ok(None) => {
                    return Err(format!(
                        "{} did not stop before the update timeout",
                        self.provider
                    ));
                }
                Err(error) => {
                    return Err(format!(
                        "could not observe {} process exit: {error}",
                        self.provider
                    ));
                }
            }
        }
    }
}

impl Drop for JsonLineProcess {
    fn drop(&mut self) {
        // Closing stdin delivers EOF, which both CLIs treat as shutdown, and
        // the reader threads exit with their pipes. The forced fallback drops
        // the Job Object, which terminates the npm shim and its descendant
        // together instead of stranding the descendant.
        let _ = self.shutdown(Duration::from_millis(250), true);
    }
}

#[cfg(test)]
mod tests;
