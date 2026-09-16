use std::ops::Range;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use nmt_config::colors::Colors;

use crate::terminal_tab::frame::TerminalLine;
use crate::terminal_tab::frame_source::{ItemViewport, TerminalFrameSource};
use crate::terminal_tab::pane_model::PaneController;
use crate::terminal_tab::pane_model::test_session::{TestOutput, controller, streaming_controller};
use crate::terminal_tab::session::{HostEvent, TerminalSessionConfig};

#[test]
fn visible_history_survives_a_pending_revision_refresh() {
    for rows in [0..12, 60..72] {
        let (mut model, output) = history_controller();

        let before = loaded_history(&model, rows.clone());

        for update in 0..8 {
            feed_history_output(
                &mut model,
                &output,
                format!("\r\x1b[KCompiling package-{update}\r\n\x1b]9;4;1;50\x1b\\Building")
                    .as_bytes(),
            );

            let after = model
                .source
                .live_history_lines(rows.clone(), model.theme.foreground);

            assert_eq!(
                after.len(),
                before.len(),
                "pending pages must keep their displayed rows"
            );

            for ((before_row, before), (after_row, after)) in before.iter().zip(&after) {
                assert_eq!(after_row, before_row);
                assert_eq!(after.text(), before.text());
            }
        }
    }
}

#[test]
fn history_display_reloads_after_reflow_and_theme_changes() {
    let (mut model, _) = history_controller();

    loaded_history(&model, 0..12);

    assert!(model.source.session.resize(80, 6, 640, 108));

    wait_until(|| model.source.session.snapshot().cols() == 80);

    model.refresh_frame();

    assert!(
        model
            .source
            .live_history_lines(0..12, model.theme.foreground)
            .is_empty()
    );

    loaded_history(&model, 0..12);

    let theme = model.source.snapshot.theme_revision();

    let colors = Colors {
        foreground: [0.1, 0.8, 0.3, 1.0],
        ..Colors::default()
    };

    assert!(model.source.session.set_theme_colors(&colors));

    wait_until(|| model.source.session.snapshot().theme_revision() != theme);

    model.refresh_frame();

    assert!(
        model
            .source
            .live_history_lines(0..12, model.theme.foreground)
            .is_empty()
    );

    loaded_history(&model, 0..12);
}

#[test]
fn history_display_drops_rows_after_clear_or_command_completion() {
    for boundary in [
        "\x1b]133;K\x07\x1b[2J\x1b[3J\x1b[H",
        "\x1b]133;D;0\x07\x1b]133;A\x07> \x1b]133;B\x07next\r\n\x1b]133;C\x07",
    ] {
        let (mut model, output) = history_controller();

        loaded_history(&model, 0..12);

        let replacement = format!("{boundary}{}", history_output("replacement", 100));

        feed_history_output(&mut model, &output, replacement.as_bytes());

        assert!(
            model
                .source
                .live_history_lines(0..12, model.theme.foreground)
                .is_empty()
        );

        let after = loaded_history(&model, 0..12);

        assert!(
            after
                .iter()
                .any(|(_, line)| line.text().contains("replacement"))
        );
        assert!(
            after
                .iter()
                .all(|(_, line)| !line.text().contains("history row"))
        );
    }
}

#[test]
fn history_display_adopts_new_pages_and_drops_nonvisible_rows() {
    let (mut model, output) = history_controller();

    loaded_history(&model, 0..12);
    feed_history_output(&mut model, &output, history_output("later", 100).as_bytes());

    assert!(
        model
            .source
            .live_history_lines(128..140, model.theme.foreground)
            .is_empty()
    );

    let after = loaded_history(&model, 128..140);

    assert!(
        after
            .iter()
            .all(|(row, line)| (128..140).contains(row) && line.text().contains("later"))
    );

    feed_history_output(&mut model, &output, b"\x1b[3J");

    assert!(
        model
            .source
            .live_history_lines(128..140, model.theme.foreground)
            .is_empty()
    );
}

fn history_output(prefix: &str, count: usize) -> String {
    (0..count)
        .map(|row| format!("{prefix} row {row}\r\n"))
        .collect()
}

fn history_controller() -> (PaneController, Arc<TestOutput>) {
    let initial = format!(
        "\x1b]133;A\x07\x1b]133;B\x07\x1b]133;C\x07\x1b]133;D;0\x07\
         \x1b]133;A\x07> \x1b]133;B\x07build\r\n\x1b]133;C\x07{}",
        history_output("history", 80),
    );

    let (model, _, output) = streaming_controller(initial.as_bytes(), true);

    (model, output)
}

fn loaded_history(model: &PaneController, rows: Range<u64>) -> Vec<(u64, TerminalLine)> {
    let mut lines = Vec::new();

    wait_until(|| {
        lines = model
            .source
            .live_history_lines(rows.clone(), model.theme.foreground);

        lines.len() == (rows.end - rows.start) as usize
    });

    lines
}

fn feed_history_output(model: &mut PaneController, output: &TestOutput, bytes: &[u8]) {
    let title = format!("history-update-{}", model.source.snapshot.revision());

    let mut bytes = bytes.to_vec();

    bytes.extend_from_slice(format!("\x1b]0;{title}\x07").as_bytes());
    output.push(&bytes);

    wait_until(|| model.source.session.title() == title);

    model.refresh_frame();
}

fn wait_until(mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(2);

    while !ready() {
        assert!(Instant::now() < deadline, "terminal update did not arrive");

        thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn bad_shell_returns_error() {
    let config = TerminalSessionConfig {
        shell: Some("this-shell-does-not-exist-xyz.exe".to_string()),
        ..TerminalSessionConfig::default()
    };

    let err = TerminalFrameSource::new(config, 1, None, Colors::default())
        .err()
        .expect("bad shell must fail");

    assert!(err.contains("PtySpawn"));
}

#[test]
fn osc_notification_drains_into_shared_exact_notification_lifecycle() {
    use nmt_agent::{
        AgentActivityPolicy, AgentMonitor, AgentRoute, AgentRuntimeStatus, request_native_delivery,
    };

    let event = HostEvent::Notification {
        title: "T".repeat(300),
        body: "B".repeat(5_000),
    };

    let route = AgentRoute::parse("osc-route").unwrap();

    let mut monitor = AgentMonitor::new("process");

    monitor.register_route(
        route.clone(),
        AgentActivityPolicy::ExpireAfterInactivity,
        Instant::now(),
    );

    let HostEvent::Notification { title, body } = event else {
        panic!("expected OSC notification host event");
    };

    monitor.notify(&route, &title, &body);

    let notification = monitor.notification(&route).unwrap().clone();

    assert_eq!(notification.title.chars().count(), 256);
    assert_eq!(notification.body.chars().count(), 4_096);
    assert_eq!(monitor.project([&route]).status, AgentRuntimeStatus::Idle);
    assert!(request_native_delivery(None, &route));
    assert!(monitor.mark_native_requested(&route, &notification.id));
    assert!(
        monitor
            .acknowledge(&route, &notification.id)
            .visible_changed
    );
    assert_eq!(monitor.project([&route]).unread_count, 0);
}

#[test]
fn frozen_item_loads_rows_and_reuses_images_without_a_window() {
    let stream = b"\x1b]133;A\x07\x1b]133;B\x07\x1b]133;C\x07\x1b]133;D;0\x07\
        \x1b]133;A\x07> \x1b]133;B\x07echo hi\r\n\x1b]133;C\x07hi\r\n\
        \x1b_Ga=T,f=32,s=1,v=1,i=1,p=9,c=1,r=2;/wAA/w==\x1b\\\r\n\
        \x1b]133;D;0\x07\x1b]133;A\x07> \x1b]133;B\x07";

    let (model, _) = controller(stream, true);
    let count = model.source.session.block_store().lock().items().len();

    let item = (0..count)
        .find(|&item| model.source.session.block_command(item).as_deref() == Some("echo hi"))
        .expect("the completed command has a frozen item");

    let viewport = ItemViewport {
        top: 0.0,
        height: 108.0,
        cell_height: 18.0,
        pad_rows: 1.0,
    };

    let deadline = Instant::now() + Duration::from_secs(2);

    let view = loop {
        let view = model.source.frozen_block_view(
            item,
            &viewport,
            None,
            &model.duration_labels,
            model.theme.foreground,
        );

        if !view.rows.is_empty() && !view.images.is_empty() {
            break view;
        }

        assert!(
            Instant::now() < deadline,
            "frozen rows and images did not load"
        );

        thread::sleep(Duration::from_millis(1));
    };

    assert!(view.rows.iter().any(|row| row.line.text().contains("hi")));
    assert!(view.items_chrome.iter().any(|chrome| {
        chrome
            .header
            .as_deref()
            .is_some_and(|header| header.starts_with("echo hi"))
    }));

    let second = model.source.frozen_block_view(
        item,
        &viewport,
        None,
        &model.duration_labels,
        model.theme.foreground,
    );

    assert_eq!(view.images.len(), second.images.len());
    assert!(Arc::ptr_eq(
        &view.images[0].generation,
        &second.images[0].generation
    ));
    assert!(
        model
            .source
            .frozen_block_view(
                count + 1,
                &viewport,
                None,
                &model.duration_labels,
                model.theme.foreground
            )
            .rows
            .is_empty()
    );
}
