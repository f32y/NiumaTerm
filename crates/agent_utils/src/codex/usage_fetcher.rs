//! Fetches the active Codex account's remaining rate limits through the Codex CLI.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::os::windows::process::CommandExt as _;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, from_str};
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

use crate::subprocess::KillOnCloseJob;
use crate::usage::{
    FIVE_HOUR_WINDOW_MINUTES, UsageResetCredits, UsageSnapshot, UsageWindow, WEEKLY_WINDOW_MINUTES,
    parse_timestamp_millis,
};

const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

pub fn fetch() -> Result<UsageSnapshot, String> {
    // The app-server is the Codex-owned boundary for OAuth refresh, account
    // selection, and backend compatibility; NiumaTerm only reads its RPC result.
    let mut child = Command::new("cmd.exe")
        .args([
            "/D",
            "/C",
            "codex",
            "-s",
            "read-only",
            "-a",
            "untrusted",
            "app-server",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|err| format!("failed to start Codex app-server: {err}"))?;

    // The npm `codex.cmd` shim starts a Node descendant. Kill-on-close
    // containment guarantees the whole tree dies on timeout or error paths;
    // killing only cmd.exe would strand the descendant holding the output
    // pipes, and with it the reader threads.
    let job = KillOnCloseJob::attach_or_kill(&mut child)?;

    let mut stdin = child.stdin.take().ok_or("Codex stdin unavailable")?;
    let stdout = child.stdout.take().ok_or("Codex stdout unavailable")?;
    let mut stderr = child.stderr.take().ok_or("Codex stderr unavailable")?;

    let (line_tx, line_rx) = mpsc::channel();

    let stdout_reader = thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if line_tx.send(line).is_err() {
                break;
            }
        }
    });

    let stderr_reader = thread::spawn(move || {
        let mut text = String::new();

        let _ = stderr.read_to_string(&mut text);

        text
    });

    let result = (|| {
        writeln!(
            stdin,
            "{}",
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"clientInfo":{"name":"NiumaTerm","version":"0.1.0"}}}"#,
        )
        .map_err(|err| format!("failed to initialize Codex app-server: {err}"))?;

        stdin
            .flush()
            .map_err(|err| format!("failed to flush Codex request: {err}"))?;

        let deadline = Instant::now() + FETCH_TIMEOUT;

        let mut requested_limits = false;

        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());

            if remaining.is_zero() {
                return Err("Codex app-server timed out".to_string());
            }

            let line = line_rx
                .recv_timeout(remaining)
                .map_err(|_| "Codex app-server timed out".to_string())?
                .map_err(|err| format!("failed to read Codex response: {err}"))?;

            let Ok(message) = from_str::<Value>(&line) else {
                continue;
            };

            match message["id"].as_u64() {
                Some(1) if !requested_limits => {
                    writeln!(
                        stdin,
                        "{}",
                        r#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#,
                    )
                    .and_then(|_| {
                        writeln!(
                            stdin,
                            "{}",
                            r#"{"jsonrpc":"2.0","id":2,"method":"account/rateLimits/read","params":{}}"#,
                        )
                    })
                    .and_then(|_| stdin.flush())
                    .map_err(|err| format!("failed to request Codex rate limits: {err}"))?;

                    requested_limits = true;
                }
                Some(2) => return parse_rate_limits(&message),
                _ => {}
            }
        }
    })()
    .map(UsageSnapshot::with_updated_now);

    // Closing stdin lets app-server observe EOF and exit cleanly together
    // with its shim; the bounded wait gives it that chance before force
    // termination.
    drop(stdin);

    let cleanup_deadline = Instant::now() + Duration::from_secs(2);

    while child.try_wait().ok().flatten().is_none() && Instant::now() < cleanup_deadline {
        thread::sleep(Duration::from_millis(10));
    }

    // Dropping the job terminates the shim and its descendant together, so
    // the output pipes always close and the reader joins below cannot hang.
    drop(job);
    let _ = child.kill();
    let _ = child.wait();

    drop(line_rx);

    let _ = stdout_reader.join();

    let stderr = stderr_reader.join().unwrap_or_default();

    result.map_err(|err| {
        let stderr = stderr.trim();

        if stderr.is_empty() {
            err
        } else {
            format!("{err}: {stderr}")
        }
    })
}

fn parse_rate_limits(message: &Value) -> Result<UsageSnapshot, String> {
    if let Some(error) = message["error"]["message"].as_str() {
        return Err(error.to_string());
    }

    let limits = &message["result"]["rateLimits"];

    let window_for_duration = |duration_mins: u32| {
        ["primary", "secondary"].into_iter().find_map(|name| {
            let window = &limits[name];
            if window["windowDurationMins"].as_u64() != Some(u64::from(duration_mins)) {
                return None;
            }

            let used = window["usedPercent"].as_f64()?;
            let mut usage = UsageWindow::new(
                (100.0 - used).clamp(0.0, 100.0).round() as u8,
                duration_mins,
            );
            usage.resets_at = parse_timestamp_millis(&window["resetsAt"]);
            Some(usage)
        })
    };

    let usage = UsageSnapshot {
        five_hour: window_for_duration(FIVE_HOUR_WINDOW_MINUTES),
        weekly: window_for_duration(WEEKLY_WINDOW_MINUTES),
        plan_type: limits["planType"].as_str().map(str::to_owned),
        reset_credits: parse_reset_credits(&message["result"]["rateLimitResetCredits"]),
        ..UsageSnapshot::default()
    };

    if usage.is_unavailable() {
        return Err("Codex response did not include rate limits".to_string());
    }

    Ok(usage)
}

fn parse_reset_credits(value: &Value) -> Option<UsageResetCredits> {
    let available_count = value["availableCount"].as_u64()?;
    let next_expires_at = parse_timestamp_millis(&value["nextExpiresAt"]).or_else(|| {
        value["credits"].as_array().and_then(|credits| {
            credits
                .iter()
                .filter(|credit| credit["status"].as_str() == Some("available"))
                .filter_map(|credit| parse_timestamp_millis(&credit["expiresAt"]))
                .min()
        })
    });

    Some(UsageResetCredits {
        available_count,
        next_expires_at,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn formats_remaining_codex_windows() {
        let response = json!({
            "id": 2,
            "result": {
                "rateLimits": {
                    "primary": { "usedPercent": 12.4, "windowDurationMins": 10080 },
                    "secondary": { "usedPercent": 67.6, "windowDurationMins": 300 }
                }
            }
        });
        assert_eq!(
            parse_rate_limits(&response).unwrap(),
            UsageSnapshot {
                five_hour: Some(UsageWindow::new(32, FIVE_HOUR_WINDOW_MINUTES)),
                weekly: Some(UsageWindow::new(88, WEEKLY_WINDOW_MINUTES)),
                ..UsageSnapshot::default()
            }
        );
    }

    #[test]
    fn rejects_missing_windows() {
        let response = json!({ "id": 2, "result": { "rateLimits": {} } });
        assert!(parse_rate_limits(&response).is_err());
    }

    #[test]
    fn keeps_the_available_window() {
        let response = json!({
            "id": 2,
            "result": {
                "rateLimits": {
                    "primary": { "usedPercent": 12.4, "windowDurationMins": 10080 }
                }
            }
        });
        assert_eq!(
            parse_rate_limits(&response).unwrap(),
            UsageSnapshot {
                weekly: Some(UsageWindow::new(88, WEEKLY_WINDOW_MINUTES)),
                ..UsageSnapshot::default()
            }
        );
    }

    #[test]
    fn keeps_reset_plan_and_reset_credit_metadata() {
        let response = json!({
            "id": 2,
            "result": {
                "rateLimits": {
                    "planType": "plus",
                    "primary": {
                        "usedPercent": 25,
                        "windowDurationMins": 300,
                        "resetsAt": 1_770_000_000
                    }
                },
                "rateLimitResetCredits": {
                    "availableCount": 2,
                    "credits": [
                        { "status": "spent", "expiresAt": 1_770_000_010 },
                        { "status": "available", "expiresAt": "2026-02-02T02:40:00Z" }
                    ]
                }
            }
        });

        let usage = parse_rate_limits(&response).unwrap();
        assert_eq!(usage.plan_type.as_deref(), Some("plus"));
        assert_eq!(usage.five_hour.unwrap().resets_at, Some(1_770_000_000_000));
        assert_eq!(
            usage.reset_credits,
            Some(UsageResetCredits {
                available_count: 2,
                next_expires_at: Some(1_770_000_000_000),
            })
        );
    }
}
