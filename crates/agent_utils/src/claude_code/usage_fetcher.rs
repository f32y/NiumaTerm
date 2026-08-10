//! Fetches remaining Claude Code subscription limits through OAuth with an
//! interactive Claude usage-panel fallback.

use std::ffi::OsStr;
use std::fs::File;
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use std::{env, thread};

use nmt_platform::{ChildEvent, EventedPty as _, ProcessReadWrite as _};
use reqwest::StatusCode;
use reqwest::blocking::Client;
use serde::Deserialize;
use serde_json::Value;

use crate::hook_store::home_dir;
use crate::usage::{
    FIVE_HOUR_WINDOW_MINUTES, UsageSnapshot, UsageWindow, WEEKLY_WINDOW_MINUTES,
    parse_timestamp_millis,
};

const OAUTH_FETCH_TIMEOUT: Duration = Duration::from_secs(10);
const CLI_FETCH_TIMEOUT: Duration = Duration::from_secs(25);
const CLI_STARTUP_DELAY: Duration = Duration::from_secs(2);
const CLI_SETTLE_DELAY: Duration = Duration::from_secs(2);
const CLI_ENTER_INTERVAL: Duration = Duration::from_millis(800);
const CLI_POLL_INTERVAL: Duration = Duration::from_millis(25);
const MAX_OUTPUT_BYTES: u64 = 128 * 1024;
const MAX_CLI_OUTPUT_BYTES: usize = 100 * 1024;
const MAX_CREDENTIALS_BYTES: u64 = 128 * 1024;
const OAUTH_USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const OAUTH_BETA_HEADER: &str = "oauth-2025-04-20";
const CLAUDE_CODE_USER_AGENT: &str = "claude-code/2.1.0";
const CANCELLED_ERROR: &str = "Claude usage request cancelled";
const CLI_STOP_MARKERS: &[&str] = &[
    "current week (all models)",
    "current week (opus)",
    "current week (sonnet only)",
    "current week (sonnet)",
    "weekly limits",
    "weekly limit",
    "weekly usage",
    "7-day",
    "current session",
    "failed to load usage data",
];

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
    fable_weekly: Option<OAuthUsageWindow>,
    fable_seven_day: Option<OAuthUsageWindow>,
    seven_day_fable: Option<OAuthUsageWindow>,
}

#[derive(Deserialize)]
struct OAuthUsageWindow {
    utilization: Option<f64>,
    used_percentage: Option<f64>,
    resets_at: Option<Value>,
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
    status == StatusCode::UNAUTHORIZED || status.is_server_error()
}

fn parse_oauth_usage(bytes: &[u8]) -> Result<UsageSnapshot, String> {
    let response: OAuthUsageResponse = serde_json::from_slice(bytes)
        .map_err(|_| "Claude OAuth usage response was not valid JSON".to_string())?;
    let usage = UsageSnapshot {
        five_hour: oauth_window(response.five_hour.as_ref(), FIVE_HOUR_WINDOW_MINUTES),
        weekly: oauth_window(response.seven_day.as_ref(), WEEKLY_WINDOW_MINUTES),
        fable_weekly: oauth_window(
            response
                .fable_weekly
                .as_ref()
                .or(response.fable_seven_day.as_ref())
                .or(response.seven_day_fable.as_ref()),
            WEEKLY_WINDOW_MINUTES,
        ),
        ..UsageSnapshot::default()
    };

    if usage.is_unavailable() {
        Err("Claude OAuth usage response did not include subscription limits".to_string())
    } else {
        Ok(usage)
    }
}

fn oauth_window(window: Option<&OAuthUsageWindow>, window_minutes: u32) -> Option<UsageWindow> {
    let window = window?;
    let mut usage = UsageWindow::new(remaining_percentage(Some(window))?, window_minutes);
    usage.resets_at = window.resets_at.as_ref().and_then(parse_timestamp_millis);
    Some(usage)
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

pub fn fetch() -> Result<UsageSnapshot, String> {
    fetch_with_cancel(&AtomicBool::new(false))
}

pub fn fetch_with_cancel(cancelled: &AtomicBool) -> Result<UsageSnapshot, String> {
    let result = match fetch_via_oauth(cancelled) {
        Ok(usage) => Ok(usage),
        Err(OAuthFetchError::Final(error)) => Err(error),
        Err(OAuthFetchError::Fallback(oauth_error)) => match fetch_via_cli(cancelled) {
            Ok(usage) => Ok(usage),
            Err(cli_error) if cli_error == CANCELLED_ERROR => Err(cli_error),
            Err(cli_error) => Err(format!(
                "Claude OAuth usage unavailable: {oauth_error}; interactive CLI fallback failed: {cli_error}"
            )),
        },
    };

    result.map(UsageSnapshot::with_updated_now)
}

fn fetch_via_cli(cancelled: &AtomicBool) -> Result<UsageSnapshot, String> {
    if cancelled.load(Ordering::Relaxed) {
        return Err(CANCELLED_ERROR.to_string());
    }

    let working_directory = Some(env::temp_dir().to_string_lossy().into_owned());
    let environment_overrides = vec![("TERM".to_string(), "xterm-256color".to_string())];
    // A real terminal is required because current Claude versions render
    // subscription limits only through the interactive `/usage` panel.
    let mut pty = nmt_platform::create_managed_pty_with_env(
        "cmd.exe",
        vec!["/D".to_string(), "/C".to_string(), "claude".to_string()],
        &working_directory,
        120,
        40,
        &environment_overrides,
        Some("Claude Usage"),
    )
    .map_err(|err| format!("could not start interactive Claude usage session: {err}"))?;

    let started_at = Instant::now();
    let deadline = started_at + CLI_FETCH_TIMEOUT;
    let mut output = Vec::new();
    let mut usage_sent = false;
    let mut trust_accepted = false;
    let mut palette_confirmed = false;
    let mut next_enter_at = None;
    let mut settle_at = None;

    loop {
        if cancelled.load(Ordering::Relaxed) {
            return Err(CANCELLED_ERROR.to_string());
        }

        let pipe_closed = drain_pty_output(&mut pty, &mut output)?;
        let now = Instant::now();

        if !usage_sent && now.duration_since(started_at) >= CLI_STARTUP_DELAY {
            write_pty(&mut pty, b"/usage\r", "Claude usage command")?;
            usage_sent = true;
            next_enter_at = Some(now + CLI_ENTER_INTERVAL);
        }

        let clean = strip_terminal_sequences(&String::from_utf8_lossy(&output));
        let lower = clean.to_ascii_lowercase();

        if !trust_accepted
            && ["do you trust", "trust the files", "safety check"]
                .iter()
                .any(|prompt| lower.contains(prompt))
        {
            write_pty(&mut pty, b"y\r", "Claude trust prompt response")?;
            trust_accepted = true;
        }

        if usage_sent
            && !palette_confirmed
            && (lower.contains("show plan") || lower.contains("usage limits"))
        {
            write_pty(&mut pty, b"\r", "Claude usage palette response")?;
            palette_confirmed = true;
        }

        if usage_sent
            && settle_at.is_none()
            && CLI_STOP_MARKERS.iter().any(|marker| lower.contains(marker))
        {
            settle_at = Some(now + CLI_SETTLE_DELAY);
        }

        if let Some(next_enter) = next_enter_at
            && now >= next_enter
            && settle_at.is_none()
        {
            write_pty(&mut pty, b"\r", "Claude usage panel advance")?;
            next_enter_at = Some(now + CLI_ENTER_INTERVAL);
        }

        if settle_at.is_some_and(|settle| now >= settle) {
            return parse_output(&clean);
        }

        if pipe_closed || matches!(pty.next_child_event(), Some(ChildEvent::Exited)) {
            return parse_output(&clean)
                .map_err(|_| "Claude exited before the usage panel rendered".to_string());
        }

        if now >= deadline {
            return parse_output(&clean)
                .map_err(|_| "Claude usage panel did not render before timeout".to_string());
        }

        thread::sleep(CLI_POLL_INTERVAL);
    }
}

fn drain_pty_output(pty: &mut nmt_platform::Pty, output: &mut Vec<u8>) -> Result<bool, String> {
    let mut buffer = [0u8; 8 * 1024];
    loop {
        match pty.reader().read(&mut buffer) {
            Ok(0) => return Ok(false),
            Ok(read) => append_bounded(output, &buffer[..read], MAX_CLI_OUTPUT_BYTES),
            Err(err) if err.kind() == ErrorKind::BrokenPipe => return Ok(true),
            Err(err) => return Err(format!("failed to read Claude usage panel: {err}")),
        }
    }
}

fn append_bounded(output: &mut Vec<u8>, bytes: &[u8], max_bytes: usize) {
    if bytes.len() >= max_bytes {
        output.clear();
        output.extend_from_slice(&bytes[bytes.len() - max_bytes..]);
        return;
    }

    let overflow = output
        .len()
        .saturating_add(bytes.len())
        .saturating_sub(max_bytes);
    if overflow > 0 {
        output.drain(..overflow);
    }
    output.extend_from_slice(bytes);
}

fn write_pty(pty: &mut nmt_platform::Pty, bytes: &[u8], description: &str) -> Result<(), String> {
    pty.writer()
        .write_all(bytes)
        .map_err(|err| format!("failed to write {description}: {err}"))
}

pub fn parse_output(output: &str) -> Result<UsageSnapshot, String> {
    let normalized = strip_terminal_sequences(&output.replace("\r\n", "\n").replace('\r', "\n"));
    let lines: Vec<&str> = normalized.lines().collect();
    let usage = UsageSnapshot {
        five_hour: cli_window(&lines, is_session_label, FIVE_HOUR_WINDOW_MINUTES),
        weekly: cli_window(&lines, is_weekly_label, WEEKLY_WINDOW_MINUTES),
        fable_weekly: cli_window(&lines, is_fable_label, WEEKLY_WINDOW_MINUTES),
        ..UsageSnapshot::default()
    };

    if usage.is_unavailable() {
        Err("Claude usage panel did not include subscription limits".to_string())
    } else {
        Ok(usage)
    }
}

fn cli_window(
    lines: &[&str],
    matches_label: fn(&str) -> bool,
    window_minutes: u32,
) -> Option<UsageWindow> {
    let mut usage = UsageWindow::new(
        extract_remaining_after_label(lines, matches_label)?,
        window_minutes,
    );
    usage.reset_description = extract_reset_description_after_label(lines, matches_label);
    Some(usage)
}

fn extract_remaining_after_label(lines: &[&str], matches_label: fn(&str) -> bool) -> Option<u8> {
    for (index, line) in lines.iter().enumerate() {
        if !matches_label(line) {
            continue;
        }

        for (offset, candidate) in lines[index..lines.len().min(index + 12)].iter().enumerate() {
            if offset > 0 && is_section_label(candidate) {
                break;
            }
            if let Some(remaining) = parse_remaining_percentage(candidate) {
                return Some(remaining);
            }
        }
    }
    None
}

fn extract_reset_description_after_label(
    lines: &[&str],
    matches_label: fn(&str) -> bool,
) -> Option<String> {
    for (index, line) in lines.iter().enumerate() {
        if !matches_label(line) {
            continue;
        }

        for (offset, candidate) in lines[index..lines.len().min(index + 12)].iter().enumerate() {
            if offset > 0 && is_section_label(candidate) {
                break;
            }
            let lower = candidate.to_ascii_lowercase();
            let Some(reset_index) = lower.find("reset") else {
                continue;
            };
            let description = candidate[reset_index..].trim();
            if !description.is_empty() {
                return Some(description.to_string());
            }
        }
    }
    None
}

fn parse_remaining_percentage(line: &str) -> Option<u8> {
    let lower = line.to_ascii_lowercase();
    for (percent_index, _) in lower.match_indices('%') {
        let prefix = &lower[..percent_index];
        let number_start = prefix
            .bytes()
            .rposition(|byte| !byte.is_ascii_digit() && byte != b'.')
            .map_or(0, |index| index + 1);
        let Ok(value) = prefix[number_start..].parse::<f64>() else {
            continue;
        };
        if !value.is_finite() || !(0.0..=100.0).contains(&value) {
            continue;
        }

        let Some(word) = lower[percent_index + 1..]
            .split(|ch: char| !ch.is_ascii_alphabetic())
            .find(|word| !word.is_empty())
        else {
            continue;
        };
        let remaining = match word {
            "used" | "consumed" => 100.0 - value,
            "left" | "remaining" | "available" => value,
            _ => continue,
        };
        return Some(remaining.round() as u8);
    }
    None
}

fn is_session_label(line: &str) -> bool {
    line.to_ascii_lowercase().contains("current session")
}

fn is_weekly_label(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    !lower.contains("fable")
        && (lower.contains("current week")
            || lower.contains("weekly limit")
            || lower.contains("weekly usage")
            || lower.contains("weekly rate limit")
            || lower.contains("7-day")
            || lower.contains("7 day"))
}

fn is_fable_label(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.contains("fable")
        && (lower.trim() == "fable"
            || lower.contains("current week")
            || lower.contains("weekly")
            || lower.contains("7-day")
            || lower.contains("7 day"))
}

fn is_section_label(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    is_session_label(&lower) || is_weekly_label(&lower) || is_fable_label(&lower)
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

#[cfg(test)]
mod tests {
    use super::*;

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
                five_hour: Some(UsageWindow::new(88, FIVE_HOUR_WINDOW_MINUTES)),
                weekly: Some(UsageWindow::new(66, WEEKLY_WINDOW_MINUTES)),
                ..UsageSnapshot::default()
            }
        );
        assert_eq!(
            parse_oauth_usage(br#"{"five_hour":{"utilization":120}}"#).unwrap(),
            UsageSnapshot {
                five_hour: Some(UsageWindow::new(0, FIVE_HOUR_WINDOW_MINUTES)),
                ..UsageSnapshot::default()
            }
        );
        assert_eq!(
            parse_oauth_usage(br#"{"fable_seven_day":{"utilization":12,"resets_at":1770000000}}"#)
                .unwrap(),
            UsageSnapshot {
                fable_weekly: Some(UsageWindow {
                    remaining_percentage: 88,
                    window_minutes: WEEKLY_WINDOW_MINUTES,
                    resets_at: Some(1_770_000_000_000),
                    reset_description: None,
                }),
                ..UsageSnapshot::default()
            }
        );
    }

    #[test]
    fn oauth_fallback_is_limited_to_recoverable_statuses() {
        assert!(oauth_status_allows_cli_fallback(StatusCode::UNAUTHORIZED));
        assert!(oauth_status_allows_cli_fallback(
            StatusCode::INTERNAL_SERVER_ERROR
        ));
        assert!(!oauth_status_allows_cli_fallback(StatusCode::FORBIDDEN));
        assert!(!oauth_status_allows_cli_fallback(
            StatusCode::TOO_MANY_REQUESTS
        ));
        assert!(!oauth_status_allows_cli_fallback(StatusCode::NOT_FOUND));
    }

    #[test]
    fn parses_interactive_usage_panel_with_split_labels_and_values() {
        let output = "\u{1b}[32mCurrent session\u{1b}[0m\r\n████ 97% used\r\nCurrent week (all models)\r\n17% consumed\r\nCurrent week (Fable)\r\n32% used\r\n";
        assert_eq!(
            parse_output(output).unwrap(),
            UsageSnapshot {
                five_hour: Some(UsageWindow::new(3, FIVE_HOUR_WINDOW_MINUTES)),
                weekly: Some(UsageWindow::new(83, WEEKLY_WINDOW_MINUTES)),
                fable_weekly: Some(UsageWindow::new(68, WEEKLY_WINDOW_MINUTES)),
                ..UsageSnapshot::default()
            }
        );
    }

    #[test]
    fn accepts_remaining_wording_and_weekly_label_variants() {
        let output = "Current session: 62% left\nWeekly limits\n41.6% available\n";
        assert_eq!(
            parse_output(output).unwrap(),
            UsageSnapshot {
                five_hour: Some(UsageWindow::new(62, FIVE_HOUR_WINDOW_MINUTES)),
                weekly: Some(UsageWindow::new(42, WEEKLY_WINDOW_MINUTES)),
                ..UsageSnapshot::default()
            }
        );
    }

    #[test]
    fn keeps_cli_reset_descriptions_with_their_windows() {
        let output = "Current session\n62% left\nResets in 2h 5m\nCurrent week (all models)\n42% left\nResets Tue 9:00 AM\n";
        let usage = parse_output(output).unwrap();

        assert_eq!(
            usage.five_hour.unwrap().reset_description.as_deref(),
            Some("Resets in 2h 5m")
        );
        assert_eq!(
            usage.weekly.unwrap().reset_description.as_deref(),
            Some("Resets Tue 9:00 AM")
        );
    }

    #[test]
    fn keeps_cli_output_bounded_to_the_newest_bytes() {
        let mut output = b"old".to_vec();
        append_bounded(&mut output, b"-new-data", 8);
        assert_eq!(output, b"new-data");
    }
}
