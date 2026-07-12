//! Minimal Codex Hook adapter. This path runs before logging, config, primary
//! election, session restore, or GPUI initialization and always fails open.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{AgentEvent, AgentEventInput, AgentEventKind};

#[derive(Serialize, Deserialize)]
pub struct RawCodexHookEnvelope {
    pub action: String,
    pub version: u32,
    pub token: String,
    pub route: String,
    pub payload: Value,
}

impl RawCodexHookEnvelope {
    pub fn into_event(self, expected_token: &str) -> Option<AgentEvent> {
        (self.action == "codex_hook").then_some(())?;
        normalize_codex(
            self.payload,
            &self.route,
            &self.token,
            self.version,
            expected_token,
        )
    }
}

fn normalize_codex(
    payload: Value,
    route: &str,
    token: &str,
    version: u32,
    expected_token: &str,
) -> Option<AgentEvent> {
    let session_id = payload.get("session_id")?.as_str()?;
    let hook = payload.get("hook_event_name")?.as_str()?;
    let (kind, title, body) = match hook {
        "SessionStart" => (AgentEventKind::SessionStarted, "", ""),
        "UserPromptSubmit" => (AgentEventKind::PromptSubmitted, "", ""),
        "PreToolUse" => (AgentEventKind::ToolStarted, "", ""),
        "PermissionRequest" => (
            AgentEventKind::PermissionRequested,
            "Codex needs input",
            payload
                .pointer("/tool_input/description")
                .and_then(Value::as_str)
                .or_else(|| payload.get("tool_name").and_then(Value::as_str))
                .unwrap_or("Codex is waiting for permission"),
        ),
        "PostToolUse" => (AgentEventKind::ToolFinished, "", ""),
        "Stop" => (
            AgentEventKind::Stopped,
            "Codex finished",
            payload
                .get("last_assistant_message")
                .and_then(Value::as_str)
                .unwrap_or("Codex completed the turn"),
        ),
        _ => return None,
    };
    let turn_id = payload.get("turn_id").and_then(Value::as_str);
    AgentEvent::validate(
        AgentEventInput {
            route,
            token,
            version,
            agent: "codex",
            session_id,
            turn_id,
            kind,
            title,
            body,
        },
        expected_token,
    )
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AGENT_HOOK_PROTOCOL_VERSION;

    fn fixture_events() -> Vec<Value> {
        let fixture: Value =
            serde_json::from_str(include_str!("../tests/fixtures/codex-0.144.1.json")).unwrap();
        fixture["events"].as_array().unwrap().clone()
    }

    #[test]
    fn captured_six_events_normalize_and_round_trip() {
        let kinds = [
            AgentEventKind::SessionStarted,
            AgentEventKind::PromptSubmitted,
            AgentEventKind::ToolStarted,
            AgentEventKind::PermissionRequested,
            AgentEventKind::ToolFinished,
            AgentEventKind::Stopped,
        ];
        for (payload, expected) in fixture_events().into_iter().zip(kinds) {
            let event = normalize_codex(
                payload,
                "test-route",
                "test-token",
                AGENT_HOOK_PROTOCOL_VERSION,
                "test-token",
            )
            .unwrap();
            assert_eq!(event.kind, expected);
        }
    }

    #[test]
    fn unknown_event_and_missing_turn_fail_open() {
        let unknown = serde_json::json!({"hook_event_name":"Other","session_id":"s"});
        let missing_turn = serde_json::json!({"hook_event_name":"Stop","session_id":"s"});
        assert!(normalize_codex(unknown, "route", "token", 1, "token").is_none());
        assert!(normalize_codex(missing_turn, "route", "token", 1, "token").is_none());
    }

    #[test]
    fn unknown_fields_and_unicode_presentation_are_safe() {
        let payload = serde_json::json!({
            "hook_event_name": "PermissionRequest",
            "session_id": "s",
            "turn_id": "t",
            "unknown": {"nested": true},
            "tool_input": {"description": "允\u{0}许".repeat(3000)}
        });
        let event = normalize_codex(
            payload,
            "route",
            "token",
            AGENT_HOOK_PROTOCOL_VERSION,
            "token",
        )
        .unwrap();
        assert_eq!(event.kind, AgentEventKind::PermissionRequested);
        assert!(event.body.chars().count() <= 4_096);
        assert!(!event.body.contains('\0'));
    }
}
