#[test]
fn filling_adds_missing_windows_and_keeps_the_ones_already_read() {
    let panel = super::UsageSnapshot {
        five_hour: Some(super::UsageWindow::new(10, 300)),
        weekly: Some(super::UsageWindow::new(20, 10_080)),
        fable_weekly: Some(super::UsageWindow::new(30, 10_080)),
        ..super::UsageSnapshot::default()
    };
    let endpoint = super::UsageSnapshot {
        five_hour: Some(super::UsageWindow::new(88, 300)),
        ..super::UsageSnapshot::default()
    };

    let filled = endpoint.filled_from(&panel);

    // The window the endpoint read stands; the ones it left out arrive.
    assert_eq!(filled.five_hour, Some(super::UsageWindow::new(88, 300)));
    assert_eq!(filled.weekly, panel.weekly);
    assert_eq!(filled.fable_weekly, panel.fable_weekly);
}

use crate::usage::*;

#[test]
fn projects_available_and_missing_windows_for_compact_display() {
    assert_eq!(
        UsageSnapshot {
            five_hour: Some(UsageWindow::new(97, FIVE_HOUR_WINDOW_MINUTES)),
            ..UsageSnapshot::default()
        }
        .compact_values(),
        ["97%", "—"]
    );
}

#[test]
fn provider_metadata_does_not_count_as_a_quota_window() {
    assert!(
        UsageSnapshot {
            plan_type: Some("plus".to_string()),
            ..UsageSnapshot::default()
        }
        .is_unavailable()
    );
}

#[test]
fn the_compact_window_falls_back_to_the_weekly_limit() {
    let both = UsageSnapshot {
        five_hour: Some(UsageWindow::new(90, FIVE_HOUR_WINDOW_MINUTES)),
        weekly: Some(UsageWindow::new(40, WEEKLY_WINDOW_MINUTES)),
        ..UsageSnapshot::default()
    };
    let weekly_only = UsageSnapshot {
        weekly: Some(UsageWindow::new(40, WEEKLY_WINDOW_MINUTES)),
        ..UsageSnapshot::default()
    };

    // The shorter window is the limit about to be hit, so it wins where a plan
    // defines both; a plan that defines only the longer one still reports.
    assert_eq!(both.compact_window(), both.five_hour.as_ref());
    assert_eq!(weekly_only.compact_window(), weekly_only.weekly.as_ref());
    assert_eq!(UsageSnapshot::default().compact_window(), None);
}
