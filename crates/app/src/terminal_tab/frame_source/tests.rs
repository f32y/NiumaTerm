use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use nmt_config::colors::Colors;

use crate::terminal_tab::frame_source::{ItemViewport, TerminalFrameSource};
use crate::terminal_tab::pane_model::test_session::controller;
use crate::terminal_tab::session::{HostEvent, TerminalSessionConfig};

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
