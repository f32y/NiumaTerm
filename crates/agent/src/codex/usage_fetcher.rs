//! Fetches the active Codex account's remaining rate limits through the Codex CLI.

#[cfg(test)]
#[path = "usage_fetcher_tests.rs"]
mod usage_fetcher_tests;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::{Value, json};

use crate::launcher::AgentCli;
use crate::message_memory::OUTPUT_FAILURE_METHOD;
use crate::subprocess::JsonLineProcess;
use crate::usage::{
    FIVE_HOUR_WINDOW_MINUTES, UsageResetCredits, UsageSnapshot, UsageWindow, WEEKLY_WINDOW_MINUTES,
    parse_timestamp_millis,
};

const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

pub fn fetch(launcher: &AgentCli, cancelled: &AtomicBool) -> Result<UsageSnapshot, String> {
    if cancelled.load(Ordering::Relaxed) {
        return Err("Codex usage request cancelled".into());
    }

    let command = launcher.command([
        // The explicit approval override also replaces obsolete policies in
        // the user's CLI settings before app-server validates them.
        "-c",
        "approval_policy=never",
        "-s",
        "read-only",
        "-a",
        "never",
        "app-server",
    ]);

    let (tx, rx) = mpsc::sync_channel(64);
    let stderr = Arc::new(Mutex::new(String::new()));

    let mut process = JsonLineProcess::spawn_with_stdout_closed(
        command,
        &format!("{} app-server", launcher.executable()),
        "Codex",
        move |message| {
            let _ = tx.send(message);
        },
        {
            let stderr = Arc::clone(&stderr);
            let launcher = launcher.clone();

            move |line| *stderr.lock() = launcher.redact(&line)
        },
        || {},
    )?;

    let result =
        read_rate_limits(&mut process, &rx, cancelled).map(UsageSnapshot::with_updated_now);

    drop(rx);

    let timeout = if cancelled.load(Ordering::Relaxed) {
        Duration::ZERO
    } else {
        Duration::from_millis(250)
    };

    let _ = process.shutdown(timeout, true);

    result.map_err(|error| {
        let stderr = stderr.lock();
        let stderr = stderr.trim();
        let error = launcher.redact(&error);

        if stderr.is_empty() {
            error
        } else {
            format!("{error}: {stderr}")
        }
    })
}

fn read_rate_limits(
    process: &mut JsonLineProcess,
    messages: &mpsc::Receiver<Value>,
    cancelled: &AtomicBool,
) -> Result<UsageSnapshot, String> {
    process
        .write_line(json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"clientInfo": {"name": "NiumaTerm", "version": "0.1.0"}}
        }))
        .map_err(|error| error.to_string())?;

    let deadline = Instant::now() + FETCH_TIMEOUT;

    let mut requested_limits = false;

    loop {
        if cancelled.load(Ordering::Relaxed) {
            return Err("Codex usage request cancelled".into());
        }

        let remaining = deadline.saturating_duration_since(Instant::now());

        if remaining.is_zero() {
            return Err("Codex app-server timed out".into());
        }

        let message = match messages.recv_timeout(remaining.min(Duration::from_millis(50))) {
            Ok(message) => message,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("Codex app-server closed its output".into());
            }
        };

        if message["method"] == OUTPUT_FAILURE_METHOD {
            return Err(message["params"]["message"]
                .as_str()
                .unwrap_or("Invalid Codex response")
                .into());
        }

        match message["id"].as_u64() {
            Some(1) if !requested_limits => {
                if let Some(error) = message["error"]["message"].as_str() {
                    return Err(error.into());
                }

                process.write_line(json!({"jsonrpc": "2.0", "method": "initialized", "params": {}}))
                    .and_then(|_| process.write_line(json!({"jsonrpc": "2.0", "id": 2, "method": "account/rateLimits/read", "params": {}})))
                    .map_err(|error| error.to_string())?;

                requested_limits = true;
            }
            Some(2) if requested_limits => return parse_rate_limits(&message),
            _ => {}
        }
    }
}

fn parse_rate_limits(message: &Value) -> Result<UsageSnapshot, String> {
    if let Some(error) = message["error"]["message"].as_str() {
        return Err(error.to_string());
    }

    let limits = &message["result"]["rateLimits"];

    let window_for_duration = |duration_mins: u32| {
        ["primary", "secondary"].into_iter().find_map(|name| {
            let window = &limits[name];

            if window["windowDurationMins"].as_u64() != Some(duration_mins.into()) {
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
