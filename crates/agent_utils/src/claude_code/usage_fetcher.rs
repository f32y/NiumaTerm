//! Fetches remaining Claude Code subscription limits through OAuth with a CLI fallback.

use std::ffi::{OsStr, c_void};
use std::fs::File;
use std::io::{Error, Read};
use std::os::windows::io::AsRawHandle as _;
use std::os::windows::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use std::{env, mem, ptr, thread};

use reqwest::StatusCode;
use reqwest::blocking::Client;
use serde::Deserialize;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject,
};
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

use crate::hook_store::home_dir;
use crate::usage::UsageSnapshot;

const CLI_FETCH_TIMEOUT: Duration = Duration::from_secs(15);
const OAUTH_FETCH_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(25);
const MAX_OUTPUT_BYTES: u64 = 128 * 1024;
const MAX_CREDENTIALS_BYTES: u64 = 128 * 1024;
const OAUTH_USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const OAUTH_BETA_HEADER: &str = "oauth-2025-04-20";
const CLAUDE_CODE_USER_AGENT: &str = "claude-code/2.1.0";
const CANCELLED_ERROR: &str = "Claude usage request cancelled";

#[derive(Debug)]
enum OAuthFetchError {
    Fallback(String),
    Final(String),
}

#[derive(Deserialize)]
struct ClaudeCredentials {
    #[serde(rename = "claudeAiOauth")]
    claude_ai_oauth: Option<ClaudeOauthCredentials>,
}

#[derive(Deserialize)]
struct ClaudeOauthCredentials {
    #[serde(rename = "accessToken")]
    access_token: Option<String>,
}

#[derive(Deserialize)]
struct OAuthUsageResponse {
    five_hour: Option<OAuthUsageWindow>,
    seven_day: Option<OAuthUsageWindow>,
}

#[derive(Deserialize)]
struct OAuthUsageWindow {
    utilization: Option<f64>,
    used_percentage: Option<f64>,
}

fn oauth_credentials_path() -> Option<PathBuf> {
    let config_dir = env::var_os("CLAUDE_CONFIG_DIR");
    credentials_path(config_dir.as_deref(), home_dir().as_deref())
}

fn credentials_path(config_dir: Option<&OsStr>, home: Option<&Path>) -> Option<PathBuf> {
    config_dir
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .or_else(|| home.map(|path| path.join(".claude")))
        .map(|path| path.join(".credentials.json"))
}

fn parse_oauth_token(bytes: &[u8]) -> Result<String, String> {
    let credentials: ClaudeCredentials = serde_json::from_slice(bytes)
        .map_err(|_| "Claude OAuth credentials were not valid JSON".to_string())?;
    credentials
        .claude_ai_oauth
        .and_then(|oauth| oauth.access_token)
        .map(|token| token.trim().to_owned())
        .filter(|token| !token.is_empty())
        .ok_or_else(|| "Claude OAuth access token unavailable".to_string())
}

fn read_oauth_token() -> Result<String, String> {
    // Claude Code's Windows subscription login is persisted in its config
    // directory. Environment API keys are intentionally excluded because the
    // OAuth usage endpoint rejects them even though they authenticate API calls.
    let path = oauth_credentials_path()
        .ok_or_else(|| "Claude credentials directory unavailable".to_string())?;
    let file = File::open(path).map_err(|_| "Claude OAuth credentials unavailable".to_string())?;
    let bytes = read_bounded_bytes(file, MAX_CREDENTIALS_BYTES, "Claude credentials")?;
    parse_oauth_token(&bytes)
}

fn oauth_status_allows_cli_fallback(status: StatusCode) -> bool {
    !matches!(
        status,
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN | StatusCode::TOO_MANY_REQUESTS
    )
}

fn parse_oauth_usage(bytes: &[u8]) -> Result<UsageSnapshot, String> {
    let response: OAuthUsageResponse = serde_json::from_slice(bytes)
        .map_err(|_| "Claude OAuth usage response was not valid JSON".to_string())?;
    let usage = UsageSnapshot {
        five_hour_remaining: remaining_percentage(response.five_hour.as_ref()),
        weekly_remaining: remaining_percentage(response.seven_day.as_ref()),
    };

    if usage.is_unavailable() {
        Err("Claude OAuth usage response did not include subscription limits".to_string())
    } else {
        Ok(usage)
    }
}

fn remaining_percentage(window: Option<&OAuthUsageWindow>) -> Option<u8> {
    let window = window?;
    let used = window.utilization.or(window.used_percentage)?;
    used.is_finite()
        .then(|| (100.0 - used.clamp(0.0, 100.0)).round() as u8)
}

fn fetch_via_oauth(cancelled: &AtomicBool) -> Result<UsageSnapshot, OAuthFetchError> {
    if cancelled.load(Ordering::Relaxed) {
        return Err(OAuthFetchError::Final(CANCELLED_ERROR.to_string()));
    }

    let token = read_oauth_token().map_err(OAuthFetchError::Fallback)?;
    let client = Client::builder()
        .timeout(OAUTH_FETCH_TIMEOUT)
        .build()
        .map_err(|_| {
            OAuthFetchError::Fallback("could not initialize Claude OAuth client".to_string())
        })?;

    let mut response = client
        .get(OAUTH_USAGE_URL)
        .bearer_auth(token)
        .header("anthropic-beta", OAUTH_BETA_HEADER)
        .header("User-Agent", CLAUDE_CODE_USER_AGENT)
        .send()
        .map_err(|_| OAuthFetchError::Fallback("Claude OAuth usage request failed".to_string()))?;

    if cancelled.load(Ordering::Relaxed) {
        return Err(OAuthFetchError::Final(CANCELLED_ERROR.to_string()));
    }

    let status = response.status();
    if !status.is_success() {
        let message = format!(
            "Claude OAuth usage request returned HTTP {}",
            status.as_u16()
        );
        // Authentication, authorization, and rate-limit responses are already
        // authoritative provider answers. Starting Claude would issue another
        // provider request and could hide the account state reported here.
        return Err(if oauth_status_allows_cli_fallback(status) {
            OAuthFetchError::Fallback(message)
        } else {
            OAuthFetchError::Final(message)
        });
    }

    let bytes = read_bounded_bytes(
        &mut response,
        MAX_OUTPUT_BYTES,
        "Claude OAuth usage response",
    )
    .map_err(OAuthFetchError::Fallback)?;
    if cancelled.load(Ordering::Relaxed) {
        return Err(OAuthFetchError::Final(CANCELLED_ERROR.to_string()));
    }

    parse_oauth_usage(&bytes).map_err(OAuthFetchError::Fallback)
}

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
    match fetch_via_oauth(cancelled) {
        Ok(usage) => Ok(usage),
        Err(OAuthFetchError::Final(error)) => Err(error),
        Err(OAuthFetchError::Fallback(oauth_error)) => match fetch_via_cli(cancelled) {
            Ok(usage) => Ok(usage),
            Err(cli_error) if cli_error == CANCELLED_ERROR => Err(cli_error),
            Err(cli_error) => Err(format!(
                "Claude OAuth usage unavailable: {oauth_error}; CLI fallback failed: {cli_error}"
            )),
        },
    }
}

fn fetch_via_cli(cancelled: &AtomicBool) -> Result<UsageSnapshot, String> {
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
    let deadline = Instant::now() + CLI_FETCH_TIMEOUT;

    let status = loop {
        if cancelled.load(Ordering::Relaxed) {
            let _ = child.kill();
            break Err(CANCELLED_ERROR.to_string());
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

fn read_bounded(reader: impl Read) -> Result<Vec<u8>, String> {
    read_bounded_bytes(reader, MAX_OUTPUT_BYTES, "Claude usage output")
}

fn read_bounded_bytes(
    mut reader: impl Read,
    max_bytes: u64,
    description: &str,
) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    reader
        .by_ref()
        .take(max_bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(|err| format!("failed to read {description}: {err}"))?;

    if bytes.len() as u64 > max_bytes {
        return Err(format!("{description} exceeded {max_bytes} bytes"));
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
    fn credentials_path_prefers_an_explicit_claude_config_dir() {
        assert_eq!(
            credentials_path(
                Some(OsStr::new(r"D:\profiles\claude")),
                Some(Path::new(r"C:\Users\test")),
            ),
            Some(PathBuf::from(r"D:\profiles\claude\.credentials.json"))
        );
        assert_eq!(
            credentials_path(None, Some(Path::new(r"C:\Users\test"))),
            Some(PathBuf::from(r"C:\Users\test\.claude\.credentials.json"))
        );
    }

    #[test]
    fn reads_only_a_nonempty_claude_oauth_access_token() {
        assert_eq!(
            parse_oauth_token(
                br#"{"claudeAiOauth":{"accessToken":"  oauth-token  "},"apiKey":"ignored"}"#
            )
            .unwrap(),
            "oauth-token"
        );
        assert!(parse_oauth_token(br#"{"anthropicApiKey":"api-key"}"#).is_err());
        assert!(parse_oauth_token(br#"{"claudeAiOauth":{"accessToken":" "}}"#).is_err());
    }

    #[test]
    fn maps_oauth_windows_to_remaining_integer_percentages() {
        assert_eq!(
            parse_oauth_usage(
                br#"{"five_hour":{"utilization":12},"seven_day":{"used_percentage":34.4}}"#
            )
            .unwrap(),
            UsageSnapshot {
                five_hour_remaining: Some(88),
                weekly_remaining: Some(66),
            }
        );
        assert_eq!(
            parse_oauth_usage(br#"{"five_hour":{"utilization":120}}"#).unwrap(),
            UsageSnapshot {
                five_hour_remaining: Some(0),
                weekly_remaining: None,
            }
        );
        assert!(parse_oauth_usage(br#"{"fable_weekly":{"utilization":12}}"#).is_err());
    }

    #[test]
    fn auth_and_rate_limit_statuses_do_not_allow_cli_fallback() {
        for status in [
            StatusCode::UNAUTHORIZED,
            StatusCode::FORBIDDEN,
            StatusCode::TOO_MANY_REQUESTS,
        ] {
            assert!(!oauth_status_allows_cli_fallback(status));
        }
        for status in [StatusCode::BAD_REQUEST, StatusCode::INTERNAL_SERVER_ERROR] {
            assert!(oauth_status_allows_cli_fallback(status));
        }
    }

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
