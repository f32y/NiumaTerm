use crate::agent::usage::*;

#[test]
fn compact_projection_keeps_provider_and_window_order() {
    let view = AgentUsageView {
        codex: UsageSnapshot {
            five_hour: Some(UsageWindow::new(25, 300)),
            weekly: Some(UsageWindow::new(80, 10_080)),
            ..UsageSnapshot::default()
        },
        claude: UsageSnapshot {
            five_hour: Some(UsageWindow::new(3, 300)),
            ..UsageSnapshot::default()
        },
        codex_refresh: ProviderRefresh::default(),
        claude_refresh: ProviderRefresh::default(),
        claude_cancel: Arc::new(AtomicBool::new(false)),
        enabled: true,
    };

    assert_eq!(
        view.accessibility_label(),
        "Agent usage remaining. Codex five hour: 25%; Codex week: 80%; Claude five hour: 3%; Claude week: —."
    );
}

#[test]
fn detail_rows_include_the_optional_fable_window() {
    let usage = UsageSnapshot {
        five_hour: Some(UsageWindow::new(75, 300)),
        weekly: Some(UsageWindow::new(55, 10_080)),
        fable_weekly: Some(UsageWindow::new(35, 10_080)),
        ..UsageSnapshot::default()
    };

    assert_eq!(
        usage_window_rows(&usage)
            .into_iter()
            .map(|row| (row.label, row.window.remaining_percentage))
            .collect::<Vec<_>>(),
        [("Session", 75), ("Weekly", 55), ("Fable weekly", 35),]
    );
    assert_eq!(format_window_duration(300), "5h");
    assert_eq!(format_window_duration(10_080), "7d");
}

#[test]
fn reset_and_update_labels_use_compact_relative_time() {
    let now = 1_000_000_000;
    let mut window = UsageWindow::new(75, 300);
    window.resets_at = Some(now + 2 * 60 * 60_000 + 5 * 60_000);
    assert_eq!(
        format_reset_label(&window, now).as_deref(),
        Some("Resets in 2h 5m")
    );

    let usage = UsageSnapshot {
        updated_at: Some(now - 7 * 60_000),
        ..UsageSnapshot::default()
    };
    assert_eq!(
        format_updated_label(&usage, false, true, now),
        "Refresh failed · updated 7m ago"
    );
}
