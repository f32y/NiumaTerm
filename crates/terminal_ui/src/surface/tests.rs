use nmt_config::local_state::TabState;

use crate::session::{HostEvent, TerminalSessionConfig};
use crate::surface::{TerminalSurface, restorable_tab_state, tab_state_with_cwd};

#[test]
fn bad_shell_returns_error() {
    let config = TerminalSessionConfig {
        shell: Some("this-shell-does-not-exist-xyz.exe".to_string()),
        ..TerminalSessionConfig::default()
    };

    let err = TerminalSurface::new(config, 1, None)
        .err()
        .expect("bad shell must fail");

    assert!(err.contains("PtySpawn"));
}

#[test]
fn tab_state_uses_last_reported_cwd() {
    let launch = TabState {
        name: None,
        user_named: false,
        shell: Some("pwsh.exe".into()),
        args: vec!["-NoLogo".into()],
        cwd: Some("C:/old".into()),
        agent: None,
        agent_profile: None,
        panes: None,
    };

    let state = tab_state_with_cwd(&launch, Some("C:/new".into()));

    assert_eq!(state.shell, launch.shell);
    assert_eq!(state.args, launch.args);
    assert_eq!(state.cwd.as_deref(), Some("C:/new"));
}

#[test]
fn restorable_tab_state_keeps_original_launch_command() {
    let config = TerminalSessionConfig {
        shell: Some("pwsh.exe".to_string()),
        working_dir: Some("C:/Projects/example".to_string()),
        ..TerminalSessionConfig::default()
    };

    let state = restorable_tab_state(&config);
    let integrated = config.with_shell_integration();

    assert_eq!(state.shell.as_deref(), Some("pwsh.exe"));
    assert!(state.args.is_empty());
    assert_eq!(state.cwd.as_deref(), Some("C:/Projects/example"));

    // PowerShell's integration rewrites the launch args, so the restorable
    // state has to be the copy taken before it. Elsewhere the shell is not one
    // with an integration and the args stay empty either way.
    #[cfg(windows)]
    assert!(!integrated.args.is_empty());

    #[cfg(unix)]
    assert!(integrated.args.is_empty());
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
