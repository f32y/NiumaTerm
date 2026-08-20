//! Shared Windows launcher semantics for agent sessions and maintenance commands.

use std::cmp::Reverse;
use std::collections::VecDeque;
use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::os::windows::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};
use std::{env, fmt, io, thread};

use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

use crate::LaunchConfig;
use crate::subprocess::KillOnCloseJob;

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

    pub fn effective_env_os(&self, target: &str) -> Option<OsString> {
        self.environment
            .iter()
            .rev()
            .find(|(name, _)| name.eq_ignore_ascii_case(target))
            .map(|(_, value)| OsString::from(value))
            .or_else(|| env::var_os(target))
    }

    /// Resolve the launcher for installation identity without changing how it
    /// is subsequently started. Resolution failures retain the configured
    /// spelling so a missing binary still receives a stable diagnostic key.
    pub fn resolved_executable(&self) -> PathBuf {
        let configured = Path::new(&self.executable);
        let resolved = if configured.components().count() > 1 || configured.is_absolute() {
            Some(configured.to_path_buf())
        } else {
            let path = self
                .effective_env_os("PATH")
                .unwrap_or_else(|| OsString::from(""));
            let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
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
        let mut command = Command::new("cmd.exe");
        command
            .args(["/D", "/C", self.executable.as_str()])
            .args(&self.arguments)
            .args(arguments)
            .envs(self.environment.iter().map(|(name, value)| (name, value)))
            .creation_flags(CREATE_NO_WINDOW);
        command
    }

    fn redact(&self, text: &str) -> String {
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

    fn redact_capped(&self, bytes: &[u8], output_limit: usize) -> (String, bool) {
        let redacted = self.redact(&String::from_utf8_lossy(bytes));
        let truncated = redacted.len() > output_limit;
        (utf8_suffix(&redacted, output_limit), truncated)
    }
}

/// Resource bounds for non-interactive probes and vendor update commands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessLimits {
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

impl Default for ProcessLimits {
    fn default() -> Self {
        Self::new(Duration::from_secs(20), 128 * 1024)
    }
}

pub struct ProcessOutput {
    pub status: ExitStatus,
    pub stdout: String,
    pub stderr: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub elapsed: Duration,
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
            .field("stdout_truncated", &self.stdout_truncated)
            .field("stderr_truncated", &self.stderr_truncated)
            .field("elapsed", &self.elapsed)
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
            stdout_truncated: false,
            stderr_truncated: false,
            elapsed: Duration::ZERO,
        }
    }
}

#[derive(Debug)]
pub enum ProcessError {
    Spawn(String),
    Containment(String),
    Wait(String),
    TimedOut { after: Duration, diagnostic: String },
    Reader(String),
}

impl fmt::Display for ProcessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn(message)
            | Self::Containment(message)
            | Self::Wait(message)
            | Self::Reader(message) => formatter.write_str(message),
            Self::TimedOut { after, diagnostic } => {
                write!(formatter, "command timed out after {}s", after.as_secs())?;
                if !diagnostic.is_empty() {
                    write!(formatter, ": {diagnostic}")?;
                }
                Ok(())
            }
        }
    }
}

impl Error for ProcessError {}

/// Run a configured launcher with bounded time and output. Reader threads keep
/// draining after their retained suffix is full so a verbose child cannot
/// deadlock on a pipe; the Job Object kills the complete owned process tree on
/// timeout or early error.
pub fn run_bounded<I, S>(
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
        ProcessError::Spawn(format!(
            "could not run configured launcher `{}`: {error}",
            launcher.executable()
        ))
    })?;
    let job = KillOnCloseJob::attach_or_kill(&mut child).map_err(ProcessError::Containment)?;

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
                let raw_diagnostic = if stderr.bytes.is_empty() {
                    String::from_utf8_lossy(&stdout.bytes)
                } else {
                    String::from_utf8_lossy(&stderr.bytes)
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
                return Err(ProcessError::Wait(format!(
                    "could not observe configured launcher exit: {error}"
                )));
            }
        }
    };
    drop(job);

    let stdout = join_reader(stdout_reader)?;
    let stderr = join_reader(stderr_reader)?;
    let raw_stdout = utf8_suffix(
        &String::from_utf8_lossy(&stdout.bytes),
        limits.max_output_bytes,
    );
    let (stdout_text, stdout_redaction_truncated) =
        launcher.redact_capped(&stdout.bytes, limits.max_output_bytes);
    let (stderr_text, stderr_redaction_truncated) =
        launcher.redact_capped(&stderr.bytes, limits.max_output_bytes);
    Ok(ProcessOutput {
        status,
        stdout: stdout_text,
        stderr: stderr_text,
        stdout_truncated: stdout.truncated
            || stdout.bytes.len() > limits.max_output_bytes
            || stdout_redaction_truncated,
        stderr_truncated: stderr.truncated
            || stderr.bytes.len() > limits.max_output_bytes
            || stderr_redaction_truncated,
        elapsed: started.elapsed(),
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

struct BoundedBytes {
    bytes: Vec<u8>,
    truncated: bool,
}

fn spawn_bounded_reader(
    mut reader: impl io::Read + Send + 'static,
    limit: usize,
) -> thread::JoinHandle<io::Result<BoundedBytes>> {
    thread::spawn(move || {
        let mut retained = VecDeque::with_capacity(limit.min(64 * 1024));
        let mut buffer = [0_u8; 8 * 1024];
        let mut truncated = false;
        loop {
            let read = reader.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            for byte in &buffer[..read] {
                if retained.len() == limit {
                    retained.pop_front();
                    truncated = true;
                }
                if limit > 0 {
                    retained.push_back(*byte);
                } else {
                    truncated = true;
                }
            }
        }
        Ok(BoundedBytes {
            bytes: retained.into(),
            truncated,
        })
    })
}

fn join_reader(
    reader: thread::JoinHandle<io::Result<BoundedBytes>>,
) -> Result<BoundedBytes, ProcessError> {
    reader
        .join()
        .map_err(|_| ProcessError::Reader("configured launcher output reader panicked".into()))?
        .map_err(|error| ProcessError::Reader(format!("could not read launcher output: {error}")))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd_launcher() -> AgentCli {
        AgentCli::new("cmd.exe", [])
    }

    #[test]
    fn bounded_runner_retains_suffix_and_redacts_environment_values() {
        let secret = "secret-value-for-test";
        let launcher = AgentCli::new(
            "cmd.exe",
            [("NMT_TEST_SECRET".to_string(), secret.to_string())],
        );
        let output = run_bounded(
            &launcher,
            ["/D", "/C", "echo 1234567890%NMT_TEST_SECRET%"],
            ProcessLimits::new(Duration::from_secs(3), 20),
        )
        .unwrap();
        assert!(output.success());
        assert!(output.stdout_truncated);
        assert!(!output.stdout.contains(secret));
        assert!(output.stdout.contains("<redacted>"));
    }

    #[test]
    fn structured_probe_parsing_precedes_diagnostic_redaction() {
        let launcher = AgentCli::new(
            "cmd.exe",
            [("NMT_TEST_VALUE".to_string(), "codex".to_string())],
        );
        let output = run_bounded(
            &launcher,
            ["/D", "/C", "echo {\"codexVersion\":\"1.2.3\"}"],
            ProcessLimits::new(Duration::from_secs(3), 256),
        )
        .unwrap();

        assert!(output.stdout.contains("<redacted>Version"));
        assert!(output.stdout_for_parsing().contains("codexVersion"));
        assert!(!format!("{output:?}").contains("codexVersion"));
    }

    #[test]
    fn bounded_runner_times_out_and_reports_bounded_diagnostics() {
        let error = run_bounded(
            &cmd_launcher(),
            ["/D", "/C", "echo before-timeout & ping -n 6 127.0.0.1 >nul"],
            ProcessLimits::new(Duration::from_millis(100), 64),
        )
        .unwrap_err();
        assert!(matches!(error, ProcessError::TimedOut { .. }));
        assert!(error.to_string().len() < 4_200);
    }
}
