#[cfg(windows)]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(windows)]
use std::sync::{Arc, mpsc};
#[cfg(windows)]
use std::{fs, thread};

#[cfg(windows)]
use tempfile::tempdir;

#[cfg(windows)]
use crate::LaunchConfig;
use serde_json::json;

use crate::codex::usage_fetcher::*;

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

#[cfg(windows)]
fn fake_launcher(script: &str) -> (tempfile::TempDir, AgentCli) {
    let directory = tempdir().unwrap();
    let path = directory.path().join("usage probe.ps1");

    fs::write(&path, script).unwrap();

    let launcher = AgentCli::from_launch(
        &LaunchConfig {
            executable: "powershell.exe".into(),
            executable_args: vec![
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-File".into(),
                path.display().to_string(),
            ],
            env: vec![("NMT_USAGE_USED".into(), "27".into())],
            ..LaunchConfig::default()
        },
        "codex",
    );

    (directory, launcher)
}

#[cfg(windows)]
#[test]
fn configured_usage_launcher_exchanges_requests_and_environment() {
    let (_directory, launcher) = fake_launcher(
        r#"
$initialize = [Console]::ReadLine() | ConvertFrom-Json
if ($initialize.method -ne 'initialize') { exit 2 }
[Console]::Out.WriteLine('{"id":1,"result":{}}')
$initialized = [Console]::ReadLine() | ConvertFrom-Json
$request = [Console]::ReadLine() | ConvertFrom-Json
if ($initialized.method -ne 'initialized' -or $request.method -ne 'account/rateLimits/read') { exit 3 }
[Console]::Out.WriteLine(('{"id":2,"result":{"rateLimits":{"primary":{"usedPercent":' + $env:NMT_USAGE_USED + ',"windowDurationMins":300}}}}'))
"#,
    );

    let usage = fetch(&launcher, &AtomicBool::new(false)).unwrap();

    assert_eq!(usage.five_hour.unwrap().remaining_percentage, 73);
    assert!(usage.updated_at.is_some());
}

#[cfg(windows)]
#[test]
fn cancelled_usage_request_stops_a_waiting_child() {
    let (directory, launcher) = fake_launcher(
        r#"
[IO.File]::WriteAllText((Join-Path $PSScriptRoot 'ready'), 'ready')
Start-Sleep -Seconds 30
"#,
    );

    let cancelled = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel();

    let worker = thread::spawn({
        let cancelled = cancelled.clone();

        move || tx.send(fetch(&launcher, &cancelled)).unwrap()
    });

    let deadline = Instant::now() + Duration::from_secs(5);

    while !directory.path().join("ready").exists() {
        assert!(Instant::now() < deadline, "child did not start");

        thread::sleep(Duration::from_millis(20));
    }

    cancelled.store(true, Ordering::Relaxed);

    let error = rx
        .recv_timeout(Duration::from_secs(1))
        .unwrap()
        .unwrap_err();

    assert!(error.contains("cancelled"));

    worker.join().unwrap();
}
