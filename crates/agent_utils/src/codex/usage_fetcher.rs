//! Fetches the active Codex account's remaining rate limits through the Codex CLI.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::os::windows::process::CommandExt as _;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub five_hour: Option<u8>,
    pub weekly: Option<u8>,
}

pub fn fetch() -> Result<Usage, String> {
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

    let mut stdin = child.stdin.take().ok_or("Codex stdin unavailable")?;
    let stdout = child.stdout.take().ok_or("Codex stdout unavailable")?;
    let mut stderr = child.stderr.take().ok_or("Codex stderr unavailable")?;
    let (line_tx, line_rx) = mpsc::channel();
    let stdout_reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if line_tx.send(line).is_err() {
                break;
            }
        }
    });
    let stderr_reader = std::thread::spawn(move || {
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
            let Ok(message) = serde_json::from_str::<serde_json::Value>(&line) else {
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
    })();

    // The npm `codex.cmd` shim starts a descendant process. Closing stdin lets
    // app-server observe EOF and exit too; killing only cmd.exe leaves the
    // descendant holding the output pipes and would strand reader threads.
    drop(stdin);
    let cleanup_deadline = Instant::now() + Duration::from_secs(2);
    while child.try_wait().ok().flatten().is_none() && Instant::now() < cleanup_deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
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

fn parse_rate_limits(message: &serde_json::Value) -> Result<Usage, String> {
    if let Some(error) = message["error"]["message"].as_str() {
        return Err(error.to_string());
    }
    let limits = &message["result"]["rateLimits"];
    let left = |duration_mins| {
        ["primary", "secondary"].into_iter().find_map(|name| {
            let window = &limits[name];
            (window["windowDurationMins"].as_u64() == Some(duration_mins))
                .then(|| window["usedPercent"].as_f64())
                .flatten()
                .map(|used| (100.0 - used).clamp(0.0, 100.0).round() as u8)
        })
    };
    let five_hour = left(5 * 60);
    let weekly = left(7 * 24 * 60);
    let usage = Usage { five_hour, weekly };
    if usage == Usage::default() {
        return Err("Codex response did not include rate limits".to_string());
    }
    Ok(usage)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_remaining_codex_windows() {
        let response = serde_json::json!({
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
            Usage {
                five_hour: Some(32),
                weekly: Some(88),
            }
        );
    }

    #[test]
    fn rejects_missing_windows() {
        let response = serde_json::json!({ "id": 2, "result": { "rateLimits": {} } });
        assert!(parse_rate_limits(&response).is_err());
    }

    #[test]
    fn keeps_the_available_window() {
        let response = serde_json::json!({
            "id": 2,
            "result": {
                "rateLimits": {
                    "primary": { "usedPercent": 12.4, "windowDurationMins": 10080 }
                }
            }
        });
        assert_eq!(
            parse_rate_limits(&response).unwrap(),
            Usage {
                five_hour: None,
                weekly: Some(88),
            }
        );
    }
}
