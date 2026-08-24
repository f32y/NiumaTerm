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
