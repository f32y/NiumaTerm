use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crate::agent_usage::*;
use crate::usage_refresh::FetchError;

#[test]
fn compact_projection_keeps_provider_and_window_order() {
    let view = AgentUsageView {
        providers: [
            Refresh::new(
                UsageSnapshot {
                    five_hour: Some(UsageWindow::new(25, 300)),
                    weekly: Some(UsageWindow::new(80, 10_080)),
                    ..UsageSnapshot::default()
                },
                Arc::new(|_: &AtomicBool| -> Result<UsageSnapshot, FetchError> {
                    panic!("presentation does not fetch")
                }),
                true,
            ),
            Refresh::new(
                UsageSnapshot {
                    five_hour: Some(UsageWindow::new(3, 300)),
                    ..UsageSnapshot::default()
                },
                Arc::new(|_: &AtomicBool| -> Result<UsageSnapshot, FetchError> {
                    panic!("presentation does not fetch")
                }),
                true,
            ),
        ],
        enabled: true,
        codex_launcher: AgentCli::new("codex", []),
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
        [
            ("Session".into(), 75),
            ("Weekly".into(), 55),
            ("Fable weekly".into(), 35),
        ]
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

#[gpui::test]
fn changing_usage_launcher_discards_the_previous_request(cx: &mut gpui::TestAppContext) {
    use gpui::AppContext as _;

    cx.update(|cx| {
        let mut settings = AppSettings::default();

        settings.agent.show_agent_usage = false;
        cx.set_global(settings);
    });

    let view = cx.new(|_| AgentUsageView {
        providers: [0, 1].map(|_| {
            Refresh::new(
                UsageSnapshot {
                    updated_at: Some(123),
                    ..UsageSnapshot::default()
                },
                Arc::new(|_: &AtomicBool| -> Result<UsageSnapshot, FetchError> {
                    panic!("cancelled source must not run")
                }),
                true,
            )
        }),
        enabled: true,
        codex_launcher: AgentCli::new("old-codex", []),
    });

    let old = view.update(cx, |view, _| view.providers[0].begin().unwrap());

    view.update(cx, |view, cx| view.on_settings_changed(cx));

    let fetched = old.run();

    view.update(cx, |view, _| {
        assert!(matches!(
            view.providers[0].complete(fetched),
            Completion::Discarded
        ));
        assert!(view.providers[0].value.updated_at.is_none());
        assert!(!view.providers[0].refreshing());
        assert_eq!(view.codex_launcher.executable(), "codex");
    });
}
