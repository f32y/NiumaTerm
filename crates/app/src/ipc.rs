//! Application message parsing and dispatch for the platform IPC transport.
//!
//! The Windows mutex and Named Pipe implementation live in `nmt_platform`.

use futures::channel::mpsc::UnboundedSender;
use nmt_agent_hook::{AgentEvent, AgentEventEnvelope, agent_process};
use nmt_platform::windows::ipc::MAX_MESSAGE_BYTES;

use crate::cli::CliAction;

pub(crate) enum IpcAction {
    Cli(CliAction),
    Agent(AgentEvent),
}

/// Run the primary's pipe server on a dedicated thread: accept one client at
/// a time, read its line(s), parse, and send each action into `tx`. Runs for
/// the process lifetime.
pub(crate) fn spawn_pipe_server(tx: UnboundedSender<IpcAction>, testing: bool) {
    nmt_platform::windows::ipc::spawn_server(testing, move |bytes| {
        match parse_message(&bytes, agent_process().hook_token()) {
            Ok(action) => return tx.unbounded_send(action).is_ok(),
            Err(error) => tracing::warn!("ignoring IPC message: {error}"),
        }
        true
    });
}

fn parse_message(bytes: &[u8], expected_token: &str) -> Result<IpcAction, String> {
    if bytes.len() > MAX_MESSAGE_BYTES {
        return Err("message exceeds 64 KiB".into());
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "message is not UTF-8")?;
    let text = text.trim_end_matches(['\r', '\n', ' ', '\t']);
    if text.is_empty() {
        return Err("empty message".into());
    }
    if text.contains(['\r', '\n']) {
        return Err("connection contains more than one message".into());
    }
    if text.starts_with('{') {
        let value: serde_json::Value =
            serde_json::from_str(text).map_err(|_| "invalid agent envelope")?;
        match value.get("action").and_then(serde_json::Value::as_str) {
            Some("agent_event") => serde_json::from_value::<AgentEventEnvelope>(value)
                .map_err(|_| "invalid agent envelope")?
                .into_event(expected_token)
                .map(IpcAction::Agent)
                .map_err(|_| "invalid agent envelope fields".into()),
            Some("codex_hook" | "claude_hook") => {
                serde_json::from_value::<nmt_agent_hook::RawAgentHookEnvelope>(value)
                    .map_err(|_| "invalid agent Hook envelope")?
                    .into_event(expected_token)
                    .map(IpcAction::Agent)
                    .ok_or_else(|| "invalid agent Hook fields".into())
            }
            _ => Err("unsupported IPC action".into()),
        }
    } else {
        crate::cli::parse_nmt_url(text)
            .map(IpcAction::Cli)
            .map_err(|_| "invalid nmt URL".into())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use nmt_agent_hook::{
        AGENT_HOOK_PROTOCOL_VERSION, AgentEvent, AgentEventInput, AgentEventKind, AgentMonitor,
        AgentRoute, AgentRuntimeStatus, COMPLETION_QUIET_WINDOW,
    };

    use super::*;

    fn raw_codex_line(route: &str, event: &str, session: &str, turn: Option<&str>) -> String {
        let mut payload = serde_json::json!({
            "hook_event_name": event,
            "session_id": session,
            "last_assistant_message": "Finished through public ingress"
        });
        if let Some(turn) = turn {
            payload["turn_id"] = turn.into();
        }
        serde_json::json!({
            "action": "codex_hook",
            "version": 1,
            "token": "token",
            "route": route,
            "payload": payload
        })
        .to_string()
    }

    fn apply_raw(monitor: &mut AgentMonitor, line: &str, now: Instant) {
        let Ok(IpcAction::Agent(event)) = parse_message(line.as_bytes(), "token") else {
            panic!("raw Hook should reach the agent reducer");
        };
        monitor.apply(event, now);
    }

    fn agent_line(token: &str) -> String {
        let event = AgentEvent::validate(
            AgentEventInput {
                route: "route",
                token,
                version: AGENT_HOOK_PROTOCOL_VERSION,
                agent: "codex",
                session_id: "session",
                turn_id: Some("turn"),
                kind: AgentEventKind::PromptSubmitted,
                title: "",
                body: "",
            },
            token,
        )
        .unwrap();
        serde_json::to_string(&AgentEventEnvelope::from_event(event, token.into())).unwrap()
    }

    #[test]
    fn parses_existing_url_and_valid_agent_envelope() {
        assert!(matches!(
            parse_message(b"nmt://action/activate\n", "token"),
            Ok(IpcAction::Cli(CliAction::Activate))
        ));
        assert!(matches!(
            parse_message(agent_line("token").as_bytes(), "token"),
            Ok(IpcAction::Agent(_))
        ));
    }

    #[test]
    fn every_agent_event_kind_round_trips() {
        for kind in [
            AgentEventKind::SessionStarted,
            AgentEventKind::PromptSubmitted,
            AgentEventKind::ToolStarted,
            AgentEventKind::PermissionRequested,
            AgentEventKind::ToolFinished,
            AgentEventKind::Stopped,
        ] {
            let turn_id = (kind != AgentEventKind::SessionStarted).then_some("turn");
            let event = AgentEvent::validate(
                AgentEventInput {
                    route: "route",
                    token: "token",
                    version: AGENT_HOOK_PROTOCOL_VERSION,
                    agent: "codex",
                    session_id: "session",
                    turn_id,
                    kind,
                    title: "title",
                    body: "body",
                },
                "token",
            )
            .unwrap();
            let line =
                serde_json::to_vec(&AgentEventEnvelope::from_event(event, "token".into())).unwrap();
            let Ok(IpcAction::Agent(decoded)) = parse_message(&line, "token") else {
                panic!("event envelope should round-trip");
            };
            assert_eq!(decoded.kind, kind);
        }
    }

    #[test]
    fn raw_codex_hook_is_authenticated_and_normalized_by_primary() {
        let line = serde_json::json!({
            "action": "codex_hook",
            "version": 1,
            "token": "token",
            "route": "route",
            "payload": {
                "hook_event_name": "UserPromptSubmit",
                "session_id": "session",
                "turn_id": "turn"
            }
        })
        .to_string();
        let Ok(IpcAction::Agent(event)) = parse_message(line.as_bytes(), "token") else {
            panic!("raw Hook should normalize");
        };
        assert_eq!(event.kind, AgentEventKind::PromptSubmitted);
        assert!(parse_message(line.as_bytes(), "wrong-token").is_err());
    }

    #[test]
    fn raw_claude_hook_is_normalized_with_session_scoped_turn() {
        let line = serde_json::json!({
            "action": "claude_hook",
            "version": 1,
            "token": "token",
            "route": "route",
            "payload": {
                "hook_event_name": "UserPromptSubmit",
                "session_id": "session"
            }
        })
        .to_string();
        let Ok(IpcAction::Agent(event)) = parse_message(line.as_bytes(), "token") else {
            panic!("raw Claude Hook should normalize");
        };
        assert_eq!(event.kind, AgentEventKind::PromptSubmitted);
        assert_eq!(event.agent, "claude");
        assert_eq!(event.turn_id.as_deref(), Some("session"));
        assert!(parse_message(line.as_bytes(), "wrong-token").is_err());
    }

    #[test]
    fn rejects_wrong_token_version_malformed_and_second_message() {
        assert!(parse_message(agent_line("old").as_bytes(), "current").is_err());
        let mut unsupported: serde_json::Value =
            serde_json::from_str(&agent_line("token")).unwrap();
        unsupported["version"] = 2.into();
        assert!(parse_message(unsupported.to_string().as_bytes(), "token").is_err());
        assert!(parse_message(b"{broken", "token").is_err());
        assert!(parse_message(&vec![b'x'; MAX_MESSAGE_BYTES + 1], "token").is_err());
        assert!(parse_message(b"nmt://action/activate\nsecond", "token").is_err());
        assert!(parse_message(b"\xff", "token").is_err());
        assert!(parse_message(agent_line("token").as_bytes(), "token").is_ok());
    }

    #[test]
    fn public_ingress_completes_and_acknowledges_exact_notification() {
        let now = Instant::now();
        let route = AgentRoute::parse("window-a:pane-1").unwrap();
        let mut monitor = AgentMonitor::new("test-process");
        assert!(monitor.register_route(route.clone(), now));

        apply_raw(
            &mut monitor,
            &raw_codex_line(route.as_str(), "UserPromptSubmit", "parent", Some("turn-1")),
            now,
        );
        assert_eq!(
            monitor.project([&route]).status,
            AgentRuntimeStatus::Running
        );

        apply_raw(
            &mut monitor,
            &raw_codex_line(route.as_str(), "Stop", "parent", Some("turn-1")),
            now,
        );
        assert!(monitor.notification(&route).is_none());
        monitor.process_due(now + COMPLETION_QUIET_WINDOW);

        let pane = monitor.project([&route]);
        let tab = monitor.project([&route]);
        let workspace = monitor.project([&route]);
        assert_eq!(pane.status, AgentRuntimeStatus::Idle);
        assert_eq!(pane.unread_count, 1);
        assert_eq!(tab, pane);
        assert_eq!(workspace, pane);
        let notification = monitor.notification(&route).unwrap().clone();
        assert_eq!(notification.body, "Finished through public ingress");

        assert!(
            monitor
                .acknowledge(&route, "stale-notification")
                .removed_notifications
                .is_empty()
        );
        let mutation = monitor.acknowledge(&route, &notification.id);
        assert_eq!(mutation.removed_notifications.len(), 1);
        assert_eq!(mutation.removed_notifications[0].id, notification.id);
        assert!(mutation.removed_notifications[0].read);
        assert_eq!(monitor.project([&route]).unread_count, 0);
        assert_eq!(monitor.project([&route]).status, AgentRuntimeStatus::Idle);
    }

    #[test]
    fn public_ingress_new_prompt_supersedes_needs_input() {
        let now = Instant::now();
        let route = AgentRoute::parse("window-a:pane-1").unwrap();
        let mut monitor = AgentMonitor::new("test-process");
        monitor.register_route(route.clone(), now);

        for (event, session, turn) in [
            ("UserPromptSubmit", "session-1", "turn-1"),
            ("PermissionRequest", "session-1", "turn-1"),
            ("UserPromptSubmit", "session-2", "turn-2"),
        ] {
            apply_raw(
                &mut monitor,
                &raw_codex_line(route.as_str(), event, session, Some(turn)),
                now,
            );
        }

        let projection = monitor.project([&route]);
        assert_eq!(projection.status, AgentRuntimeStatus::Running);
        assert_eq!(projection.unread_count, 0);
    }

    #[test]
    fn public_ingress_non_owner_stop_cannot_complete_parent() {
        let now = Instant::now();
        let route = AgentRoute::parse("window-a:pane-1").unwrap();
        let mut monitor = AgentMonitor::new("test-process");
        monitor.register_route(route.clone(), now);
        for line in [
            raw_codex_line(
                route.as_str(),
                "UserPromptSubmit",
                "parent",
                Some("parent-turn"),
            ),
            raw_codex_line(route.as_str(), "SessionStart", "child", None),
            raw_codex_line(route.as_str(), "Stop", "child", Some("child-turn")),
            raw_codex_line(route.as_str(), "Stop", "old", Some("old-turn")),
        ] {
            apply_raw(&mut monitor, &line, now);
        }
        monitor.process_due(now + COMPLETION_QUIET_WINDOW);

        let projection = monitor.project([&route]);
        assert_eq!(projection.status, AgentRuntimeStatus::Running);
        assert_eq!(projection.unread_count, 0);
    }

    #[test]
    fn public_ingress_replay_closed_route_and_replaced_id_fail_closed() {
        let now = Instant::now();
        let route = AgentRoute::parse("window-a:pane-1").unwrap();
        let mut monitor = AgentMonitor::new("test-process");
        monitor.register_route(route.clone(), now);

        apply_raw(
            &mut monitor,
            &raw_codex_line(route.as_str(), "Stop", "session", Some("turn")),
            now,
        );
        monitor.process_due(now + COMPLETION_QUIET_WINDOW);
        assert!(monitor.notification(&route).is_none());

        monitor.notify(&route, "first", "first");
        let replaced_id = monitor.notification(&route).unwrap().id.clone();
        monitor.notify(&route, "second", "second");
        assert!(!monitor.acknowledge(&route, &replaced_id).visible_changed);

        monitor.remove_route(&route);
        apply_raw(
            &mut monitor,
            &raw_codex_line(route.as_str(), "UserPromptSubmit", "session", Some("turn")),
            now,
        );
        monitor.process_due(now + COMPLETION_QUIET_WINDOW);
        assert!(monitor.notification(&route).is_none());
        assert_eq!(monitor.project([&route]).unread_count, 0);
    }
}
