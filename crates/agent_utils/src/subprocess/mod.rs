//! Shared child-process management for agent CLI subprocesses: kill-on-close
//! Job Object containment and the newline-delimited-JSON process shape used
//! by the chat sessions.

use std::io::{BufRead as _, BufReader, Write as _};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use nmt_platform::process::{KillOnCloseJob, decode_child_output};
use parking_lot::Mutex;
use serde_json::Value;
use tracing::warn;

const INPUT_QUEUE_CAPACITY: usize = 64;

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
    stdin: Option<mpsc::SyncSender<Value>>,
    /// Provider display name ("Codex", "Claude") for lifecycle error messages.
    provider: &'static str,
}

impl JsonLineProcess {
    /// Spawn `command` with all three stdio streams piped and start the two
    /// reader threads. `display_command` is the human-readable command line
    /// quoted in the spawn error.
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
        let (input_tx, input_rx) = mpsc::sync_channel::<Value>(INPUT_QUEUE_CAPACITY);
        thread::Builder::new()
            .name(format!("{provider}-stdin"))
            .spawn(move || {
                for message in input_rx {
                    if let Err(error) = writeln!(stdin, "{message}").and_then(|_| stdin.flush()) {
                        warn!(provider, %error, "agent input writer stopped");
                        // A child can close stdin without closing stdout. Terminating
                        // its tree makes the existing EOF notification reliable.
                        writer_job.lock().take();
                        break;
                    }
                }
            })
            .map_err(|error| format!("could not start {provider} input writer: {error}"))?;

        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Ok(message) = serde_json::from_str::<Value>(&line) {
                    deliver(message);
                }
            }
            on_stdout_closed();
        });

        thread::spawn(move || {
            for line in BufReader::new(stderr).split(b'\n').map_while(Result::ok) {
                on_stderr(decode_child_output(
                    line.strip_suffix(b"\r").unwrap_or(&line),
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

    /// Queue one protocol line. Transport failures terminate the process tree
    /// so reader-side EOF remains the exit-detection path.
    pub(crate) fn write_line(&mut self, message: &Value) {
        let _ = self.try_write_line(message);
    }

    pub(crate) fn try_write_line(&mut self, message: &Value) -> Result<(), String> {
        let stdin = self.stdin.as_ref().ok_or("The agent input is closed")?;
        match stdin.try_send(message.clone()) {
            Ok(()) => Ok(()),
            Err(error) => {
                let reason = match error {
                    mpsc::TrySendError::Full(_) => "The agent input queue is full",
                    mpsc::TrySendError::Disconnected(_) => "The agent input writer stopped",
                };
                // Losing one ordered request leaves correlated replies and session
                // settings inconsistent. Fail the transport instead of skipping it.
                self.stdin.take();
                self.job.lock().take();
                Err(reason.to_string())
            }
        }
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
