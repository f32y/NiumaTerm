//! Shared Windows launcher semantics for agent sessions and maintenance commands.

#[cfg(test)]
#[path = "launcher_tests.rs"]
mod launcher_tests;

use std::cmp::Reverse;
use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};
use std::{env, fmt, io, thread};

use nmt_platform::environment::override_value;
use nmt_platform::process::{
    KillOnCloseJob, decode_child_output, hidden_cmd_command, launch_env_var,
};
use thiserror::Error;

use crate::LaunchConfig;

const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// A configured executable plus its effective environment. All logical CLI
/// arguments remain separate even though `cmd.exe` is used for Windows
/// `PATHEXT` resolution of `.cmd` shims.
#[derive(Clone, PartialEq, Eq)]
pub struct AgentCli {
    executable: String,

    /// Arguments the executable itself needs, ahead of the command's own.
    arguments: Vec<String>,

    environment: Vec<(String, String)>,
}

impl fmt::Debug for AgentCli {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AgentCli")
            .field("executable", &self.executable)
            .field(
                "environment_names",
                &self
                    .environment
                    .iter()
                    .map(|(name, _)| name)
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl AgentCli {
    pub fn from_launch(launch: &LaunchConfig, default_executable: &str) -> Self {
        let executable = launch.executable.trim();

        Self {
            executable: if executable.is_empty() {
                default_executable.to_string()
            } else {
                executable.to_string()
            },
            arguments: launch.executable_args.clone(),
            environment: launch.env.clone(),
        }
    }

    /// A launcher for tests, which name an executable directly rather than
    /// through a profile.
    #[cfg(any(test, feature = "test-support"))]
    pub fn new(
        executable: impl Into<String>,
        environment: impl IntoIterator<Item = (String, String)>,
    ) -> Self {
        Self {
            executable: executable.into(),
            arguments: Vec::new(),
            environment: environment.into_iter().collect(),
        }
    }

    pub fn executable(&self) -> &str {
        &self.executable
    }

    pub fn environment(&self) -> &[(String, String)] {
        &self.environment
    }

    /// The value `target` has for this launcher: its own configuration first,
    /// then whatever a child started by [`AgentCli::command`] would inherit.
    pub fn effective_env_os(&self, target: &str) -> Option<OsString> {
        override_value(&self.environment, target)
            .map(Into::into)
            .or_else(|| launch_env_var(target))
    }

    /// Resolve the launcher for installation identity without changing how it
    /// is subsequently started. Resolution failures retain the configured
    /// spelling so a missing binary still receives a stable diagnostic key.
    pub(crate) fn resolved_executable(&self) -> PathBuf {
        let configured = Path::new(&self.executable);

        let resolved = if configured.components().count() > 1 || configured.is_absolute() {
            Some(configured.to_path_buf())
        } else {
            let path = self.effective_env_os("PATH").unwrap_or_else(|| "".into());
            let cwd = env::current_dir().unwrap_or_else(|_| ".".into());

            which::which_in(&self.executable, Some(path), cwd).ok()
        };

        resolved
            .and_then(|path| path.canonicalize().ok().or(Some(path)))
            .unwrap_or_else(|| configured.to_path_buf())
    }

    pub fn command<I, S>(&self, arguments: I) -> Command
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut command = hidden_cmd_command(&self.executable);

        command
            .args(&self.arguments)
            .args(arguments)
            .envs(self.environment.iter().map(|(name, value)| (name, value)));

        command
    }

    pub(crate) fn redact(&self, text: &str) -> String {
        let mut redacted = text.to_string();

        let mut secrets: Vec<&str> = self
            .environment
            .iter()
            .map(|(_, value)| value.as_str())
            .filter(|value| value.len() >= 4)
            .collect();

        secrets.sort_unstable_by_key(|value| Reverse(value.len()));

        secrets.dedup();

        for secret in secrets {
            redacted = redacted.replace(secret, "<redacted>");
        }

        redact_common_credentials(&redacted)
    }

    fn redaction_headroom(&self, output_limit: usize) -> usize {
        self.environment
            .iter()
            .map(|(_, value)| value.len())
            .max()
            .unwrap_or(0)
            .min(output_limit.max(1))
    }

    fn redact_capped(&self, bytes: &[u8], output_limit: usize) -> String {
        utf8_suffix(&self.redact(&decode_child_output(bytes)), output_limit)
    }
}

/// Resource bounds for non-interactive probes and vendor update commands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ProcessLimits {
    pub timeout: Duration,
    pub max_output_bytes: usize,
}

impl ProcessLimits {
    pub const fn new(timeout: Duration, max_output_bytes: usize) -> Self {
        Self {
            timeout,
            max_output_bytes,
        }
    }
}

pub(crate) struct ProcessOutput {
    pub status: ExitStatus,
    pub stdout: String,
    pub stderr: String,
    raw_stdout: SensitiveOutput,
}

struct SensitiveOutput(String);

impl fmt::Debug for SensitiveOutput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<redacted>")
    }
}

impl fmt::Debug for ProcessOutput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProcessOutput")
            .field("status", &self.status)
            .field("stdout", &self.stdout)
            .field("stderr", &self.stderr)
            .field("raw_stdout", &self.raw_stdout)
            .finish()
    }
}

impl ProcessOutput {
    pub fn success(&self) -> bool {
        self.status.success()
    }

    pub fn diagnostic(&self) -> String {
        let text = if self.stderr.trim().is_empty() {
            self.stdout.trim()
        } else {
            self.stderr.trim()
        };

        text.chars().take(4_096).collect()
    }

    /// Provider probes parse this bounded in-memory view before redaction can
    /// alter protocol field names. It is crate-private and omitted from Debug
    /// output so raw configured environment values cannot reach UI diagnostics
    /// or logs through this result.
    pub(crate) fn stdout_for_parsing(&self) -> &str {
        &self.raw_stdout.0
    }

    #[cfg(test)]
    pub(crate) fn for_test(status: ExitStatus, stdout: String, stderr: String) -> Self {
        Self {
            status,
            raw_stdout: SensitiveOutput(stdout.clone()),
            stdout,
            stderr,
        }
    }
}

/// Only the timeout is told apart by callers: it is the one outcome where
/// the child may still have done its work, so the update path reports it
/// as its own kind. Every other failure is a message.
#[derive(Debug, Error)]
pub(crate) enum ProcessError {
    #[error("{0}")]
    Failed(String),
    #[error("command timed out after {}s{}", after.as_secs(), diagnostic_suffix(diagnostic))]
    TimedOut { after: Duration, diagnostic: String },
}

fn diagnostic_suffix(diagnostic: &str) -> String {
    if diagnostic.is_empty() {
        String::new()
    } else {
        format!(": {diagnostic}")
    }
}

/// Run a configured launcher with bounded time and output. Reader threads keep
/// draining after their retained suffix is full so a verbose child cannot
/// deadlock on a pipe; the Job Object kills the complete owned process tree on
/// timeout or early error.
pub(crate) fn run_bounded<I, S>(
    launcher: &AgentCli,
    arguments: I,
    limits: ProcessLimits,
) -> Result<ProcessOutput, ProcessError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let started = Instant::now();

    let mut command = launcher.command(arguments);

    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = command.spawn().map_err(|error| {
        ProcessError::Failed(format!(
            "could not run configured launcher `{}`: {error}",
            launcher.executable()
        ))
    })?;

    let job = KillOnCloseJob::attach_or_kill(&mut child)
        .map_err(|error| ProcessError::Failed(error.to_string()))?;

    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");

    // Retain enough additional bytes to recognize a secret that straddles the
    // public output boundary, then redact before applying the configured cap.
    let capture_limit = limits
        .max_output_bytes
        .saturating_add(launcher.redaction_headroom(limits.max_output_bytes));

    let stdout_reader = spawn_bounded_reader(stdout, capture_limit);
    let stderr_reader = spawn_bounded_reader(stderr, capture_limit);

    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < limits.timeout => thread::sleep(POLL_INTERVAL),
            Ok(None) => {
                drop(job);

                let _ = child.wait();
                let stdout = join_reader(stdout_reader)?;
                let stderr = join_reader(stderr_reader)?;

                let raw_diagnostic = if stderr.is_empty() {
                    decode_child_output(&stdout)
                } else {
                    decode_child_output(&stderr)
                };

                let diagnostic = launcher.redact(&raw_diagnostic);

                return Err(ProcessError::TimedOut {
                    after: limits.timeout,
                    diagnostic: diagnostic.trim().chars().take(4_096).collect(),
                });
            }
            Err(error) => {
                drop(job);

                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();

                return Err(ProcessError::Failed(format!(
                    "could not observe configured launcher exit: {error}"
                )));
            }
        }
    };

    drop(job);

    let stdout = join_reader(stdout_reader)?;
    let stderr = join_reader(stderr_reader)?;
    let raw_stdout = utf8_suffix(&decode_child_output(&stdout), limits.max_output_bytes);

    Ok(ProcessOutput {
        status,
        stdout: launcher.redact_capped(&stdout, limits.max_output_bytes),
        stderr: launcher.redact_capped(&stderr, limits.max_output_bytes),
        raw_stdout: SensitiveOutput(raw_stdout),
    })
}

fn utf8_suffix(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }

    let mut start = value.len().saturating_sub(max_bytes);

    while start < value.len() && !value.is_char_boundary(start) {
        start += 1;
    }

    value[start..].to_string()
}

/// Drain `reader` to the end, keeping only its last `limit` bytes.
fn spawn_bounded_reader(
    mut reader: impl io::Read + Send + 'static,
    limit: usize,
) -> thread::JoinHandle<io::Result<Vec<u8>>> {
    thread::spawn(move || {
        let mut retained = VecDeque::with_capacity(limit.min(64 * 1024));
        let mut buffer = [0_u8; 8 * 1024];

        loop {
            let read = reader.read(&mut buffer)?;

            if read == 0 {
                break;
            }

            for byte in &buffer[..read] {
                if retained.len() == limit {
                    retained.pop_front();
                }

                if limit > 0 {
                    retained.push_back(*byte);
                }
            }
        }

        Ok(retained.into())
    })
}

fn join_reader(reader: thread::JoinHandle<io::Result<Vec<u8>>>) -> Result<Vec<u8>, ProcessError> {
    reader
        .join()
        .map_err(|_| ProcessError::Failed("configured launcher output reader panicked".into()))?
        .map_err(|error| ProcessError::Failed(format!("could not read launcher output: {error}")))
}

fn redact_common_credentials(text: &str) -> String {
    text.lines()
        .map(|line| {
            let lower = line.to_ascii_lowercase();

            if ["api_key", "api-key", "authorization", "bearer ", "token="]
                .iter()
                .any(|marker| lower.contains(marker))
            {
                "<redacted>".to_string()
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}
