use nmt_config::colors::Colors;

use crate::frame_source::TerminalFrameSource;
use crate::session::{HostEvent, TerminalSessionConfig};

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
    use std::time::Instant;

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
