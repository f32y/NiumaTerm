use std::time::{Duration, Instant};
use std::{env, fs, process, slice};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

use super::*;

const TOKEN: &str = "hook-secret";

fn route(value: &str) -> AgentRoute {
    AgentRoute::parse(value).unwrap()
}

#[test]
fn process_routes_are_unique_and_environment_is_exact() {
    let process = AgentProcess::new();
    let first = process.allocate_route();
    let second = process.allocate_route();
    assert_ne!(first, second);
    let environment = process.environment_for(&first);
    assert_eq!(environment.len(), 3);
    assert_eq!(environment[0], (AGENT_ROUTE_ENV.into(), first.0.clone()));
    assert_eq!(
        environment[1],
        (AGENT_HOOK_TOKEN_ENV.into(), process.hook_token.clone())
    );
    assert_eq!(environment[2], (AGENT_HOOK_VERSION_ENV.into(), "1".into()));

    process.set_hook_executable("C:\\NiumaTerm\\NiumaTermHook.exe".into());
    process.set_hook_executable("C:\\ignored\\second\\call.exe".into());
    assert_eq!(
        process.environment_for(&first)[3],
        (
            AGENT_HOOK_EXE_ENV.into(),
            "C:\\NiumaTerm\\NiumaTermHook.exe".into()
        )
    );

    process.set_testing(true);
    assert_eq!(
        process.environment_for(&first)[4],
        (AGENT_TESTING_ENV.into(), "1".into())
    );
}

#[test]
fn windows_hook_command_uses_bare_safe_path() {
    assert_eq!(
        build_windows_hook_command_for(
            r"C:\Soft\NiumaTerm\NiumaTermHook.exe",
            "codex",
            r"C:\Windows",
        )
        .unwrap(),
        r"C:\Soft\NiumaTerm\NiumaTermHook.exe codex"
    );
    assert!(
        build_windows_hook_command_for(r"C:\Hook.exe", "codex & whoami", r"C:\Windows").is_err()
    );
}

#[test]
fn windows_hook_command_encodes_unsafe_path() {
    let executable = r"C:\Program Files\Niuma'Term\%HOOK%^\NiumaTermHook.exe";
    let command = build_windows_hook_command_for(executable, "codex", r"D:\Windows").unwrap();
    assert!(command.starts_with(
        "D:/Windows/System32/WindowsPowerShell/v1.0/powershell.exe -NoProfile \
         -ExecutionPolicy Bypass -EncodedCommand "
    ));
    assert!(!command.contains(executable));
    assert!(hook_command_contains(&command, "NiumaTermHook.exe"));

    let encoded = command.rsplit_once(' ').unwrap().1;
    let bytes = STANDARD.decode(encoded).unwrap();
    let units = bytes
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .collect::<Vec<_>>();
    assert_eq!(
        String::from_utf16(&units).unwrap(),
        r"& 'C:\Program Files\Niuma''Term\%HOOK%^\NiumaTermHook.exe' codex; exit $LASTEXITCODE"
    );

    #[cfg(windows)]
    {
        let dir = env::temp_dir().join(format!("nmt hook path % ^ {}", process::id()));
        fs::create_dir_all(&dir).unwrap();
        let script = dir.join("hook.cmd");
        fs::write(&script, "@echo hook-ran\r\n@exit /b 0\r\n").unwrap();
        let command = build_windows_hook_command(script.to_str().unwrap(), "codex").unwrap();
        for (shell, args) in [
            ("powershell.exe", vec!["-NoProfile", "-Command", &command]),
            ("cmd.exe", vec!["/d", "/c", &command]),
        ] {
            let output = process::Command::new(shell).args(args).output().unwrap();
            assert_eq!(
                output.status.code(),
                Some(0),
                "{shell}: stdout={}, stderr={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains("hook-ran"));
        }
        fs::remove_dir_all(dir).unwrap();
    }
}

fn event(
    route: &AgentRoute,
    session: &str,
    turn: Option<&str>,
    kind: AgentEventKind,
) -> AgentEvent {
    AgentEvent::validate(
        AgentEventInput {
            route: route.as_str(),
            token: TOKEN,
            version: AGENT_HOOK_PROTOCOL_VERSION,
            agent: "codex",
            session_id: session,
            turn_id: turn,
            kind,
            title: "Codex",
            body: "Agent update",
        },
        TOKEN,
    )
    .unwrap()
}

fn monitor(now: Instant, routes: &[AgentRoute]) -> AgentMonitor {
    let mut monitor = AgentMonitor::new("process");
    for route in routes {
        assert!(monitor.register_route(route.clone(), now));
    }
    monitor
}

#[test]
fn validation_is_strict_and_presentation_is_bounded() {
    let r = route("pane-1");
    let input = AgentEventInput {
        route: r.as_str(),
        token: "wrong",
        version: 1,
        agent: "codex",
        session_id: "session",
        turn_id: Some("turn"),
        kind: AgentEventKind::PromptSubmitted,
        title: "title",
        body: "body",
    };
    assert_eq!(
        AgentEvent::validate(input, TOKEN),
        Err(AgentValidationError::InvalidToken)
    );
    assert!(AgentRoute::parse("").is_err());
    assert!(AgentRoute::parse(&"x".repeat(MAX_ROUTE_BYTES + 1)).is_err());
    assert_eq!(normalize_title(" a\n\0  b "), "a b");
    assert_eq!(normalize_body("a\r\nb\0c"), "a\nb c");
    let unicode = "🦀".repeat(MAX_TITLE_CHARS + 1);
    let normalized = normalize_title(&unicode);
    assert_eq!(normalized.chars().count(), MAX_TITLE_CHARS);
    assert!(normalized.is_char_boundary(normalized.len()));
}

#[test]
fn session_start_does_not_invent_running() {
    let now = Instant::now();
    let r = route("pane-1");
    let mut monitor = monitor(now, &[r.clone()]);
    monitor.apply(event(&r, "s1", None, AgentEventKind::SessionStarted), now);
    let state = monitor.pane(&r).unwrap();
    assert_eq!(state.status, AgentRuntimeStatus::Idle);
    assert_eq!(state.current_owner, None);
}

#[test]
fn prompt_claims_owner_and_replay_does_not_advance_generation() {
    let now = Instant::now();
    let r = route("pane-1");
    let mut monitor = monitor(now, &[r.clone()]);
    let prompt = event(&r, "s1", Some("opaque-b"), AgentEventKind::PromptSubmitted);
    monitor.apply(prompt.clone(), now);
    monitor.apply(prompt, now + Duration::from_secs(1));
    let state = monitor.pane(&r).unwrap();
    assert_eq!(state.status, AgentRuntimeStatus::Running);
    assert_eq!(state.turn_generation, 1);
    assert!(state.has_work_evidence);
}

#[test]
fn new_prompt_supersedes_needs_input_and_old_stop_is_ignored() {
    let now = Instant::now();
    let r = route("pane-1");
    let mut monitor = monitor(now, &[r.clone()]);
    monitor.apply(
        event(&r, "s1", Some("z"), AgentEventKind::PromptSubmitted),
        now,
    );
    monitor.apply(
        event(&r, "s1", Some("z"), AgentEventKind::PermissionRequested),
        now,
    );
    assert_eq!(
        monitor.pane(&r).unwrap().status,
        AgentRuntimeStatus::NeedsInput
    );
    assert!(monitor.notification(&r).is_some());
    monitor.apply(
        event(&r, "s2", Some("a"), AgentEventKind::PromptSubmitted),
        now,
    );
    monitor.apply(event(&r, "s1", Some("z"), AgentEventKind::Stopped), now);
    assert_eq!(
        monitor.pane(&r).unwrap().status,
        AgentRuntimeStatus::Running
    );
    assert_eq!(monitor.pane(&r).unwrap().turn_generation, 2);
    assert!(monitor.notification(&r).is_none());
    assert!(monitor.pane(&r).unwrap().pending_completion.is_none());
}

#[test]
fn nested_session_and_opaque_turn_events_cannot_steal_owner() {
    let now = Instant::now();
    let r = route("pane-1");
    let mut monitor = monitor(now, &[r.clone()]);
    monitor.apply(
        event(&r, "parent", Some("10"), AgentEventKind::PromptSubmitted),
        now,
    );
    monitor.apply(
        event(&r, "child", None, AgentEventKind::SessionStarted),
        now,
    );
    monitor.apply(
        event(&r, "parent", Some("2"), AgentEventKind::ToolFinished),
        now,
    );
    monitor.apply(event(&r, "child", Some("1"), AgentEventKind::Stopped), now);
    let owner = monitor.pane(&r).unwrap().current_owner.as_ref().unwrap();
    assert_eq!(owner.session_id, "parent");
    assert_eq!(owner.turn_id, "10");
    assert!(monitor.pane(&r).unwrap().pending_completion.is_none());
}

#[test]
fn stop_quiets_then_commits_once_and_resumed_work_cancels() {
    let now = Instant::now();
    let r = route("pane-1");
    let mut monitor = monitor(now, &[r.clone()]);
    monitor.apply(
        event(&r, "s", Some("t"), AgentEventKind::PromptSubmitted),
        now,
    );
    let stop = event(&r, "s", Some("t"), AgentEventKind::Stopped);
    monitor.apply(stop.clone(), now);
    monitor.apply(stop, now);
    assert_eq!(monitor.next_deadline(), Some(now + COMPLETION_QUIET_WINDOW));
    monitor.process_due(now + COMPLETION_QUIET_WINDOW - Duration::from_millis(1));
    assert_eq!(
        monitor.pane(&r).unwrap().status,
        AgentRuntimeStatus::Running
    );
    monitor.apply(event(&r, "s", Some("t"), AgentEventKind::ToolStarted), now);
    assert!(monitor.pane(&r).unwrap().pending_completion.is_none());
    monitor.apply(event(&r, "s", Some("t"), AgentEventKind::Stopped), now);
    monitor.process_due(now + COMPLETION_QUIET_WINDOW);
    let first_id = monitor.notification(&r).unwrap().id.clone();
    assert_eq!(monitor.pane(&r).unwrap().status, AgentRuntimeStatus::Idle);
    monitor.apply(event(&r, "s", Some("t"), AgentEventKind::Stopped), now);
    monitor.process_due(now + COMPLETION_QUIET_WINDOW * 2);
    assert_eq!(monitor.notification(&r).unwrap().id, first_id);
}

#[test]
fn stop_without_current_runtime_evidence_never_notifies() {
    let now = Instant::now();
    let r = route("pane-1");
    let mut monitor = monitor(now, &[r.clone()]);
    monitor.apply(event(&r, "s", None, AgentEventKind::SessionStarted), now);
    monitor.apply(event(&r, "s", Some("t"), AgentEventKind::Stopped), now);
    monitor.process_due(now + COMPLETION_QUIET_WINDOW);
    assert!(monitor.notification(&r).is_none());
}

#[test]
fn stale_active_state_becomes_idle_without_notification() {
    let now = Instant::now();
    let r = route("pane-1");
    let mut monitor = monitor(now, &[r.clone()]);
    monitor.apply(
        event(&r, "s", Some("t"), AgentEventKind::PromptSubmitted),
        now,
    );
    assert_eq!(
        monitor.next_deadline(),
        Some(now + ACTIVE_STATE_STALE_AFTER)
    );
    monitor.process_due(now + ACTIVE_STATE_STALE_AFTER);
    assert_eq!(monitor.pane(&r).unwrap().status, AgentRuntimeStatus::Idle);
    assert!(monitor.notification(&r).is_none());
}

#[test]
fn matching_update_reschedules_stale_expiry() {
    let now = Instant::now();
    let update = now + Duration::from_secs(60);
    let r = route("pane-1");
    let mut monitor = monitor(now, &[r.clone()]);
    monitor.apply(
        event(&r, "s", Some("t"), AgentEventKind::PromptSubmitted),
        now,
    );
    monitor.apply(
        event(&r, "s", Some("t"), AgentEventKind::ToolFinished),
        update,
    );
    assert_eq!(
        monitor.next_deadline(),
        Some(update + ACTIVE_STATE_STALE_AFTER)
    );
    monitor.process_due(now + ACTIVE_STATE_STALE_AFTER);
    assert_eq!(
        monitor.pane(&r).unwrap().status,
        AgentRuntimeStatus::Running
    );
    monitor.process_due(update + ACTIVE_STATE_STALE_AFTER);
    assert_eq!(monitor.pane(&r).unwrap().status, AgentRuntimeStatus::Idle);
}

#[test]
fn old_generation_completion_timer_cannot_complete_new_prompt() {
    let now = Instant::now();
    let r = route("pane-1");
    let mut monitor = monitor(now, &[r.clone()]);
    monitor.apply(
        event(&r, "s", Some("old"), AgentEventKind::PromptSubmitted),
        now,
    );
    monitor.apply(event(&r, "s", Some("old"), AgentEventKind::Stopped), now);
    monitor.apply(
        event(&r, "s", Some("new"), AgentEventKind::PromptSubmitted),
        now + Duration::from_millis(100),
    );
    monitor.process_due(now + COMPLETION_QUIET_WINDOW);
    assert_eq!(
        monitor.pane(&r).unwrap().status,
        AgentRuntimeStatus::Running
    );
    assert!(monitor.notification(&r).is_none());
}

#[test]
fn latest_notification_acknowledgement_and_status_are_independent() {
    let now = Instant::now();
    let r = route("pane-1");
    let mut monitor = monitor(now, &[r.clone()]);
    monitor.apply(
        event(&r, "s", Some("t"), AgentEventKind::PromptSubmitted),
        now,
    );
    monitor.apply(
        event(&r, "s", Some("t"), AgentEventKind::PermissionRequested),
        now,
    );
    let old = monitor.notification(&r).unwrap().id.clone();
    monitor.apply(
        event(&r, "s", Some("t"), AgentEventKind::PermissionRequested),
        now,
    );
    let current = monitor.notification(&r).unwrap().id.clone();
    assert_eq!(monitor.notification(&r).unwrap().native_tag.len(), 16);
    assert_eq!(monitor.notification(&r).unwrap().native_group, "NiumaTerm");
    assert_ne!(old, current);
    assert_eq!(monitor.pending_native_notifications().len(), 1);
    assert!(monitor.mark_native_requested(&r, &current));
    assert!(monitor.pending_native_notifications().is_empty());
    assert!(!monitor.mark_native_requested(&r, &old));
    assert!(!monitor.acknowledge(&r, &old).visible_changed);
    assert!(monitor.acknowledge(&r, &current).visible_changed);
    assert!(monitor.notification(&r).unwrap().read);
    assert_eq!(
        monitor.pane(&r).unwrap().status,
        AgentRuntimeStatus::NeedsInput
    );
}

#[test]
fn failed_native_operations_cannot_clear_internal_attention() {
    let now = Instant::now();
    let r = route("pane-1");
    let mut monitor = monitor(now, &[r.clone()]);
    monitor.apply(
        event(&r, "s", Some("t"), AgentEventKind::PromptSubmitted),
        now,
    );
    monitor.apply(
        event(&r, "s", Some("t"), AgentEventKind::PermissionRequested),
        now,
    );
    let id = monitor.notification(&r).unwrap().id.clone();

    // Native delivery is intentionally fire-and-forget. Recording a failed
    // attempt changes only its retry marker, never the internal projection.
    assert!(monitor.mark_native_requested(&r, &id));
    assert_eq!(monitor.project([&r]).status, AgentRuntimeStatus::NeedsInput);
    assert_eq!(monitor.project([&r]).unread_count, 1);

    // Native removal failure is likewise unable to undo the exact internal
    // acknowledgement or mutate the agent lifecycle state.
    assert!(monitor.acknowledge(&r, &id).visible_changed);
    assert_eq!(monitor.project([&r]).unread_count, 0);
    assert_eq!(monitor.project([&r]).status, AgentRuntimeStatus::NeedsInput);
}

#[test]
fn native_delivery_is_suppressed_only_for_the_exact_visible_route() {
    let target = route("target");
    let sibling = route("sibling");
    assert!(!request_native_delivery(Some(&target), &target));
    assert!(request_native_delivery(Some(&sibling), &target));
    assert!(request_native_delivery(None, &target));
}

#[test]
fn stale_gpui_active_flag_does_not_treat_minimized_window_as_visible() {
    assert!(exact_window_is_active(true, true, false));
    assert!(!exact_window_is_active(true, true, true));
    assert!(!exact_window_is_active(true, false, false));
    assert!(!exact_window_is_active(false, true, false));
}

#[test]
fn aggregation_counts_routes_and_prioritizes_needs_input() {
    let now = Instant::now();
    let a = route("a");
    let b = route("b");
    let mut monitor = monitor(now, &[a.clone(), b.clone()]);
    assert_eq!(monitor.project([&a, &b]).status, AgentRuntimeStatus::Idle);
    monitor.apply(
        event(&a, "s1", Some("t1"), AgentEventKind::PromptSubmitted),
        now,
    );
    monitor.apply(
        event(&b, "s2", Some("t2"), AgentEventKind::PromptSubmitted),
        now,
    );
    assert_eq!(
        monitor.project([&a, &b]).status,
        AgentRuntimeStatus::Running
    );
    monitor.apply(
        event(&a, "s1", Some("t1"), AgentEventKind::PermissionRequested),
        now,
    );
    monitor.apply(
        event(&b, "s2", Some("t2"), AgentEventKind::PermissionRequested),
        now,
    );
    let projection = monitor.project([&a, &b]);
    assert_eq!(projection.status, AgentRuntimeStatus::NeedsInput);
    assert_eq!(projection.unread_count, 2);
    assert_eq!(
        projection.latest_unread_text.as_deref(),
        Some("Agent update")
    );
    monitor.remove_route(&a);
    assert_eq!(monitor.project([&a, &b]).unread_count, 1);
}

#[test]
fn tab_activation_keeps_split_sibling_unread_until_exact_acknowledgement() {
    let now = Instant::now();
    let tab_one_left = route("tab-1:left");
    let tab_one_right = route("tab-1:right");
    let tab_two = route("tab-2:only");
    let mut monitor = monitor(
        now,
        &[tab_one_left.clone(), tab_one_right.clone(), tab_two.clone()],
    );
    monitor.notify(&tab_one_left, "left", "left unread");
    monitor.notify(&tab_one_right, "right", "right unread");
    monitor.notify(&tab_two, "second tab", "latest unread");

    let tab_one = monitor.project([&tab_one_left, &tab_one_right]);
    let tab_two_projection = monitor.project([&tab_two]);
    let workspace = monitor.project([&tab_one_left, &tab_one_right, &tab_two]);
    assert_eq!(tab_one.unread_count, 2);
    assert_eq!(tab_two_projection.unread_count, 1);
    assert_eq!(workspace.unread_count, 3);
    assert_eq!(
        workspace.latest_unread_text.as_deref(),
        Some("latest unread")
    );

    let left_id = monitor.notification(&tab_one_left).unwrap().id.clone();
    monitor.acknowledge(&tab_one_left, &left_id);
    assert_eq!(
        monitor
            .project([&tab_one_left, &tab_one_right])
            .unread_count,
        1
    );
    assert_eq!(
        monitor
            .project([&tab_one_left, &tab_one_right, &tab_two])
            .unread_count,
        2
    );

    let right_id = monitor.notification(&tab_one_right).unwrap().id.clone();
    monitor.acknowledge(&tab_one_right, &right_id);
    assert_eq!(
        monitor
            .project([&tab_one_left, &tab_one_right])
            .unread_count,
        0
    );
    assert_eq!(
        monitor
            .project([&tab_one_left, &tab_one_right, &tab_two])
            .unread_count,
        1
    );
}

#[test]
fn osc_style_notification_replaces_latest_without_changing_agent_state() {
    let now = Instant::now();
    let r = route("pane-1");
    let other = route("pane-2");
    let mut monitor = monitor(now, &[r.clone(), other.clone()]);
    monitor.apply(
        event(&r, "s", Some("t"), AgentEventKind::PromptSubmitted),
        now,
    );
    monitor.notify(&r, "first", "old");
    let old = monitor.notification(&r).unwrap().id.clone();
    let mutation = monitor.notify(&r, &"🦀".repeat(300), &"b".repeat(5_000));
    assert_eq!(mutation.removed_notifications[0].id, old);
    assert_eq!(
        monitor.pane(&r).unwrap().status,
        AgentRuntimeStatus::Running
    );
    assert_eq!(monitor.notification(&r).unwrap().title.chars().count(), 256);
    assert_eq!(
        monitor.notification(&r).unwrap().body.chars().count(),
        4_096
    );
    monitor.notify(&other, "other", "separate unread");
    assert_eq!(monitor.project([&r, &other]).unread_count, 2);
    assert_eq!(
        monitor
            .pane(&r)
            .unwrap()
            .current_owner
            .as_ref()
            .unwrap()
            .session_id,
        "s"
    );
}

#[test]
fn closed_route_cancels_pending_and_rejects_late_events() {
    let now = Instant::now();
    let r = route("pane-1");
    let mut monitor = monitor(now, &[r.clone()]);
    monitor.apply(
        event(&r, "s", Some("t"), AgentEventKind::PromptSubmitted),
        now,
    );
    monitor.apply(event(&r, "s", Some("t"), AgentEventKind::Stopped), now);
    monitor.remove_route(&r);
    monitor.process_due(now + COMPLETION_QUIET_WINDOW);
    monitor.apply(
        event(&r, "s", Some("t"), AgentEventKind::PromptSubmitted),
        now,
    );
    assert!(monitor.pane(&r).is_none());
    assert!(monitor.notification(&r).is_none());
}

#[test]
fn colliding_local_pane_ids_stay_isolated_across_windows_and_close_cascades() {
    let now = Instant::now();
    let local_pane_id = 1;
    let window_one = route("window-1:route-1");
    let window_two = route("window-2:route-1");
    assert_eq!(local_pane_id, 1); // Both windows may independently allocate pane 1.

    let mut first = monitor(now, slice::from_ref(&window_one));
    let mut second = monitor(now, slice::from_ref(&window_two));
    let second_event = event(
        &window_two,
        "session-2",
        Some("turn-2"),
        AgentEventKind::PromptSubmitted,
    );
    assert!(!first.apply(second_event.clone(), now).visible_changed);
    assert!(second.apply(second_event, now).visible_changed);
    assert_eq!(
        first.project([&window_one]).status,
        AgentRuntimeStatus::Idle
    );
    assert_eq!(
        second.project([&window_two]).status,
        AgentRuntimeStatus::Running
    );

    second.apply(
        event(
            &window_two,
            "session-2",
            Some("turn-2"),
            AgentEventKind::Stopped,
        ),
        now,
    );
    second.remove_route(&window_two); // pane/tab/workspace/window teardown converges here.
    second.process_due(now + COMPLETION_QUIET_WINDOW);
    assert!(second.pane(&window_two).is_none());
    assert!(second.notification(&window_two).is_none());
}

#[test]
fn background_window_notification_activation_is_exact() {
    let now = Instant::now();
    let foreground = route("window-1:route-1");
    let background = route("window-2:route-1");
    let mut foreground_monitor = monitor(now, slice::from_ref(&foreground));
    let mut background_monitor = monitor(now, slice::from_ref(&background));

    foreground_monitor.notify(&foreground, "foreground", "leave unread");
    background_monitor.notify(&background, "background", "activate me");
    let notification = background_monitor
        .notification(&background)
        .unwrap()
        .clone();
    assert!(request_native_delivery(Some(&foreground), &background));
    assert!(background_monitor.mark_native_requested(&background, &notification.id));

    assert!(
        !foreground_monitor
            .acknowledge(&background, &notification.id)
            .visible_changed
    );
    assert!(
        background_monitor
            .acknowledge(&background, &notification.id)
            .visible_changed
    );
    assert_eq!(foreground_monitor.project([&foreground]).unread_count, 1);
    assert_eq!(background_monitor.project([&background]).unread_count, 0);
}
