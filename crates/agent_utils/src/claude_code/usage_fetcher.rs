//! Fetches remaining Claude Code subscription limits through print-mode CLI output.

use std::ffi::c_void;
use std::io::{Error, Read};
use std::os::windows::io::AsRawHandle as _;
use std::os::windows::process::CommandExt as _;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use std::{mem, ptr, thread};

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject,
};
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

use crate::usage::UsageSnapshot;

const FETCH_TIMEOUT: Duration = Duration::from_secs(15);
const POLL_INTERVAL: Duration = Duration::from_millis(25);
const MAX_OUTPUT_BYTES: u64 = 128 * 1024;

struct KillOnCloseJob(HANDLE);

impl KillOnCloseJob {
    fn attach(child: &Child) -> Result<Self, String> {
        unsafe {
            let job = CreateJobObjectW(ptr::null(), ptr::null());
            if job.is_null() {
                return Err(format!(
                    "failed to create bounded Claude process job: {}",
                    Error::last_os_error()
                ));
            }

            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

            let configured = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const c_void,
                mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            ) != 0;
            if !configured {
                let error = Error::last_os_error();
                CloseHandle(job);
                return Err(format!("failed to configure Claude process job: {error}"));
            }
            if AssignProcessToJobObject(job, child.as_raw_handle() as HANDLE) == 0 {
                let error = Error::last_os_error();
                CloseHandle(job);
                return Err(format!("failed to bound Claude process tree: {error}"));
            }

            Ok(Self(job))
        }
    }
}

impl Drop for KillOnCloseJob {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

pub fn fetch() -> Result<UsageSnapshot, String> {
    fetch_with_cancel(&AtomicBool::new(false))
}

pub fn fetch_with_cancel(cancelled: &AtomicBool) -> Result<UsageSnapshot, String> {
    // Claude is installed as `claude.cmd` on Windows. cmd.exe is only the shim
    // resolver; every logical CLI argument remains a distinct Command argument.
    let mut child = Command::new("cmd.exe")
        .args(["/D", "/C", "claude", "-p", "/usage"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|err| format!("could not run `claude -p \"/usage\"`: {err}"))?;

    // Closing this job handle terminates cmd.exe and the Node descendant if a
    // timeout or cancellation occurs, avoiding readers stranded on inherited pipes.
    let job = match KillOnCloseJob::attach(&child) {
        Ok(job) => job,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
    };
    let stdout = child.stdout.take().ok_or("Claude stdout unavailable")?;
    let stderr = child.stderr.take().ok_or("Claude stderr unavailable")?;

    let stdout_reader = thread::spawn(move || read_bounded(stdout));
    let stderr_reader = thread::spawn(move || read_bounded(stderr));
    let deadline = Instant::now() + FETCH_TIMEOUT;

    let status = loop {
        if cancelled.load(Ordering::Relaxed) {
            let _ = child.kill();
            break Err("Claude usage request cancelled".to_string());
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            break Err("Claude usage request timed out".to_string());
        }

        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => thread::sleep(POLL_INTERVAL),
            Err(err) => break Err(format!("failed to wait for Claude usage output: {err}")),
        }
    };

    if status.is_err() {
        drop(job);
    }
    let _ = child.wait();

    let stdout = stdout_reader
        .join()
        .map_err(|_| "Claude stdout reader failed".to_string())??;
    let stderr = stderr_reader
        .join()
        .map_err(|_| "Claude stderr reader failed".to_string())??;
    let status = status?;

    map_process_output(status, &stdout, &stderr)
}

fn read_bounded(mut reader: impl Read) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    reader
        .by_ref()
        .take(MAX_OUTPUT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|err| format!("failed to read Claude usage output: {err}"))?;

    if bytes.len() as u64 > MAX_OUTPUT_BYTES {
        return Err(format!(
            "Claude usage output exceeded {} bytes",
            MAX_OUTPUT_BYTES
        ));
    }

    Ok(bytes)
}

fn map_process_output(
    status: ExitStatus,
    stdout: &[u8],
    _stderr: &[u8],
) -> Result<UsageSnapshot, String> {
    if !status.success() {
        // The CLI's stderr is bounded and drained to prevent process stalls,
        // but it is not surfaced because provider diagnostics may echo paths,
        // prompts, or account data. The exit status is sufficient for logs.
        return Err(format!("Claude usage command exited with {status}"));
    }

    parse_output(&String::from_utf8_lossy(stdout))
}

pub fn parse_output(output: &str) -> Result<UsageSnapshot, String> {
    let normalized = strip_terminal_sequences(&output.replace("\r\n", "\n").replace('\r', "\n"));
    let mut usage = UsageSnapshot::default();

    for line in normalized.lines() {
        if let Some(remaining) = parse_used_percent(line, "Current session:") {
            usage.five_hour_remaining = Some(remaining);
        } else if let Some(remaining) = parse_used_percent(line, "Current week (all models):") {
            usage.weekly_remaining = Some(remaining);
        }
    }

    if usage.is_unavailable() {
        Err("Claude usage output did not include subscription limits".to_string())
    } else {
        Ok(usage)
    }
}

fn parse_used_percent(line: &str, label: &str) -> Option<u8> {
    let rest = line.strip_prefix(label)?.trim_start();
    let token = rest.split_whitespace().next()?;
    let value_text = token.strip_suffix('%')?;
    if value_text.is_empty() || !value_text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let value = value_text.parse::<u8>().ok()?;
    if value > 100 || !rest[token.len()..].starts_with(" used") {
        return None;
    }
    Some(100 - value)
}

fn strip_terminal_sequences(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch != '\u{1b}' {
            output.push(ch);
            continue;
        }

        match chars.peek().copied() {
            Some('[') => {
                chars.next();
                for next in chars.by_ref() {
                    if ('@'..='~').contains(&next) {
                        break;
                    }
                }
            }
            Some(']') => {
                chars.next();
                let mut escaped = false;
                for next in chars.by_ref() {
                    if next == '\u{7}' || (escaped && next == '\\') {
                        break;
                    }
                    escaped = next == '\u{1b}';
                }
            }
            Some(_) => {
                chars.next();
            }
            None => {}
        }
    }

    output
}

#[cfg(test)]
mod tests {
    use std::os::windows::process::ExitStatusExt as _;

    use super::*;

    const SUPPLIED_OUTPUT: &str = "You are currently using your subscription to power your Claude Code usage\r\n\r\nCurrent session: 97% used · resets Aug 6, 7:30pm (Asia/Shanghai)\r\nCurrent week (all models): 17% used · resets Aug 12, 1pm (Asia/Shanghai)\r\nCurrent week (Fable): 32% used · resets Aug 12, 1pm (Asia/Shanghai)\r\n\r\nLast 24h · 369 requests · 8 sessions\r\n  83% of your usage was at >150k context\r\n";

    #[test]
    fn parses_supplied_output_as_remaining_percentages() {
        assert_eq!(
            parse_output(SUPPLIED_OUTPUT).unwrap(),
            UsageSnapshot {
                five_hour_remaining: Some(3),
                weekly_remaining: Some(83),
            }
        );
    }

    #[test]
    fn keeps_a_valid_window_and_rejects_unrelated_percentages() {
        let output = "\u{1b}[32mCurrent session:\u{1b}[0m 40% used\nCurrent week (all models): unavailable\n83% of your usage was at >150k context\n";
        assert_eq!(
            parse_output(output).unwrap(),
            UsageSnapshot {
                five_hour_remaining: Some(60),
                weekly_remaining: None,
            }
        );
    }

    #[test]
    fn rejects_malformed_and_unrelated_output() {
        assert!(
            parse_output("Current session: 101% used\nCurrent week (Fable): 20% used").is_err()
        );
        assert!(parse_output("Current session: 20% remaining\n64% came from subagents").is_err());
    }

    #[test]
    fn maps_cli_failure_without_attempting_to_parse_stdout() {
        let error = map_process_output(
            ExitStatus::from_raw(1),
            b"Current session: 10% used",
            b"claude command failed",
        )
        .unwrap_err();

        assert!(error.contains("Claude usage command exited"));
        assert!(!error.contains("claude command failed"));
    }
}
