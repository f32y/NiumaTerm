//! Shared child-process management for agent CLI subprocesses: kill-on-close
//! Job Object containment and the newline-delimited-JSON process shape used
//! by the chat sessions.

pub(crate) use crate::subprocess::input::{InputClosed, InputTicket};

pub(crate) mod requests;

mod input;

#[cfg(test)]
mod tests;

use std::future::Future;
use std::io::Write as _;
use std::process::Command;
use std::str::from_utf8;
use std::time::Duration;

use nmt_platform::process::{KillOnCloseJob, PipedChild, decode_child_output, spawn_piped};
use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::watch;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::subprocess::input::InputQueue;

/// Local notification used when a subprocess cannot safely continue reading.
pub const OUTPUT_FAILURE_METHOD: &str = "nmt/outputFailure";

/// How long a dropped process may leave on its own after its input closes
/// before its tree is ended. Short, because dropping never waits for it.
pub(crate) const DROP_SHUTDOWN_GRACE: Duration = Duration::from_millis(250);

/// A spawned agent CLI with piped stdio, kill-on-close containment, and
/// newline-delimited JSON output. Stdout lines that parse as JSON are handed
/// to `deliver`, stderr lines to `on_stderr`, each from its own runtime task.
/// Dropping `deliver` signals EOF: the closure is owned by the reader task
/// and dropped when the pipe closes. Callbacks run on runtime workers and
/// must return promptly.
pub(crate) struct JsonLineProcess {
    /// Becomes true once the root process exits. A task owns the child and
    /// waits for it, so shutdown never needs exclusive access to this value.
    exited: watch::Receiver<bool>,

    /// Ends the process tree. The exit task owns the Job Object and drops it
    /// once this fires or the root exits, so no other party needs the job.
    kill: CancellationToken,

    stdin: Option<InputQueue>,

    /// Provider display name ("Codex", "Claude") for lifecycle error messages.
    provider: &'static str,
}

impl JsonLineProcess {
    /// Spawn `command` with all three stdio streams piped and start the
    /// reader tasks. `display_command` is the human-readable command line
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
        command: Command,
        display_command: &str,
        provider: &'static str,
        deliver: impl Fn(Value) + Send + 'static,
        on_stderr: impl Fn(String) + Send + 'static,
        on_stdout_closed: impl FnOnce() + Send + 'static,
    ) -> Result<Self, String> {
        let PipedChild {
            mut child,
            mut stdin,
            stdout,
            stderr,
        } = spawn_piped(command)
            .map_err(|err| format!("could not run `{display_command}`: {err}"))?;

        let job = KillOnCloseJob::attach_spawned_or_kill(&mut child)
            .map_err(|error| error.to_string())?;

        let runtime = nmt_runtime::handle();
        let kill = CancellationToken::new();
        let (exit_sender, exited) = watch::channel(false);

        let exit_kill = kill.clone();

        runtime.spawn(async move {
            tokio::select! {
                _ = child.wait() => {}
                () = exit_kill.cancelled() => {}
            }

            // Dropping the Job Object ends whatever is left of the tree: the
            // descendants of an exited root, or the whole tree on a kill. The
            // root's exit is then observed the same way in both cases.
            drop(job);

            if let Err(error) = child.wait().await {
                warn!(provider, %error, "could not observe agent process exit");
            }

            let _ = exit_sender.send(true);
        });

        let writer_kill = kill.clone();
        let (input_tx, input_rx) = InputQueue::new();

        runtime.spawn(async move {
            while let Some(input) = input_rx.recv().await {
                // Serializing the whole batch first writes a started batch in
                // one piece, so cancellation can never split it.
                let mut lines = Vec::new();

                for message in &input.messages {
                    let _ = writeln!(lines, "{message}");
                }

                let written = async {
                    stdin.write_all(&lines).await?;

                    stdin.flush().await
                };

                if let Err(error) = written.await {
                    warn!(provider, %error, "agent input writer stopped");

                    // A child can close stdin without closing stdout. Terminating
                    // its tree makes the existing EOF notification reliable.
                    writer_kill.cancel();

                    break;
                }
            }
        });

        let reader_kill = kill.clone();

        runtime.spawn(async move {
            let mut deliver = deliver;
            let mut reader = BufReader::new(stdout);

            if let Err(message) = read_messages(&mut reader, provider, &mut deliver).await {
                warn!(provider, reason = %message, "agent protocol reader stopped");

                deliver(json!({"method": OUTPUT_FAILURE_METHOD, "params": {"message": message}}));

                reader_kill.cancel();
            }

            on_stdout_closed();
        });

        runtime.spawn(async move {
            let mut lines = BufReader::new(stderr).split(b'\n');

            while let Ok(Some(line)) = lines.next_segment().await {
                on_stderr(decode_child_output(
                    line.strip_suffix(b"\r").unwrap_or(&line),
                ));
            }
        });

        Ok(Self {
            exited,
            kill,
            stdin: Some(input_tx),
            provider,
        })
    }

    /// Queue a required control line. Failure ends the
    /// process tree so a missing reply cannot leave the session waiting forever.
    pub(crate) fn write_line(&mut self, message: Value) -> Result<(), InputClosed> {
        let result = self.try_write_line(message);

        if let Err(error) = &result {
            warn!(%error, "required agent control input was rejected");

            // Callers use this path for required replies and lifecycle controls.
            // A disconnected writer cannot deliver a required reply.
            self.abort();
        }

        result
    }

    /// Success means the writer accepted the message, not completed I/O.
    pub(crate) fn try_write_line(&mut self, message: Value) -> Result<(), InputClosed> {
        self.try_write_batch(vec![message])
    }

    /// Queue settings and prompt lines together so cancellation cannot split them.
    pub(crate) fn try_write_batch(&mut self, messages: Vec<Value>) -> Result<(), InputClosed> {
        self.write_tracked(messages).map(|_| ())
    }

    pub(crate) fn write_tracked(
        &mut self,
        messages: Vec<Value>,
    ) -> Result<InputTicket, InputClosed> {
        if !self.has_stdin() {
            return Err(InputClosed);
        }

        let result = self
            .stdin
            .as_ref()
            .ok_or(InputClosed)?
            .submit_tracked(messages);

        if result.is_err() {
            self.abort();
        }

        result
    }

    pub(crate) fn abort(&mut self) {
        self.stdin.take();

        self.kill.cancel();
    }

    /// False once shutdown has closed the protocol input or a task has ended
    /// the tree.
    pub(crate) fn has_stdin(&self) -> bool {
        self.stdin.is_some() && !self.kill.is_cancelled()
    }

    /// Close the protocol input (EOF is the CLIs' graceful-shutdown signal)
    /// and wait for the process to exit. Forced termination is opt-in because
    /// it can interrupt an active tool operation; dropping the Job Object
    /// affects only this process's tree. The returned wait owns what it needs,
    /// so callers hold neither this value nor a lock while it runs.
    pub(crate) fn shutdown(
        &mut self,
        wait: Duration,
        force: bool,
    ) -> impl Future<Output = Result<(), String>> + Send + use<> {
        drop(self.stdin.take());

        let mut exited = self.exited.clone();

        let kill = self.kill.clone();
        let provider = self.provider;

        async move {
            if timeout(wait, exited.wait_for(|exited| *exited))
                .await
                .is_ok()
            {
                return Ok(());
            }

            if !force {
                return Err(format!("{provider} did not stop before the update timeout"));
            }

            kill.cancel();

            exited
                .wait_for(|exited| *exited)
                .await
                .map(|_| ())
                .map_err(|error| format!("could not wait for {provider} to stop: {error}"))
        }
    }
}

impl Drop for JsonLineProcess {
    fn drop(&mut self) {
        // Closing stdin delivers EOF, which both CLIs treat as shutdown, and
        // the reader tasks end with their pipes. The forced fallback drops the
        // Job Object, which terminates the npm shim and its descendant together
        // instead of stranding the descendant. Dropping never waits for it.
        nmt_runtime::handle().spawn(self.shutdown(DROP_SHUTDOWN_GRACE, true));
    }
}

/// Startup wrappers may print plain-text notices before the first protocol
/// object. Once the protocol starts, skipping a malformed line could lose a
/// response or transcript item, so decoding failure ends the stream.
async fn read_messages(
    reader: &mut (impl AsyncBufRead + Unpin),
    provider: &str,
    mut deliver: impl FnMut(Value),
) -> Result<(), String> {
    let mut started = false;
    let mut first_line = true;
    let mut startup_notice_seen = false;
    let mut line = Vec::new();

    loop {
        line.clear();

        reader
            .read_until(b'\n', &mut line)
            .await
            .map_err(|error| format!("Agent output read failed: {error}"))?;

        if line.is_empty() {
            return Ok(());
        }

        let raw = if first_line {
            line.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&line)
        } else {
            &line
        };

        first_line = false;

        let raw = raw.trim_ascii();

        if raw.is_empty() {
            continue;
        }

        match serde_json::from_slice::<Value>(raw) {
            Ok(message) if message.is_object() => {
                started = true;

                deliver(message);
            }
            Ok(_) => {
                return Err(
                    "Agent protocol output must be a JSON object; the process was stopped.".into(),
                );
            }
            Err(error) => {
                let bracketed_notice =
                    [b"[warn]".as_slice(), b"[warning]", b"[info]"]
                        .iter()
                        .any(|prefix| {
                            raw.get(..prefix.len())
                                .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
                        });

                let plain_startup = !started
                    && from_utf8(raw).is_ok()
                    && raw[0] != b'{'
                    && raw[0] != b'"'
                    && (raw[0] != b'[' || bracketed_notice);

                if !plain_startup {
                    // Parser error text can include input fragments. Only the
                    // category and numeric location are safe to report here.
                    return Err(format!(
                        "Agent protocol JSON is invalid ({:?}, line {}, column {}); the process was stopped.",
                        error.classify(),
                        error.line(),
                        error.column()
                    ));
                }

                if !startup_notice_seen {
                    startup_notice_seen = true;

                    warn!(
                        provider,
                        "agent launcher produced non-protocol startup output; content omitted"
                    );
                }
            }
        }
    }
}
