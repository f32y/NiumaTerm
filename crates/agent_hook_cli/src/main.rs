#![cfg(target_os = "windows")]

use std::io::Read;
use std::time::Duration;

use nmt_agent_hook::{
    AGENT_HOOK_PROTOCOL_VERSION, AGENT_HOOK_TOKEN_ENV, AGENT_HOOK_VERSION_ENV, AGENT_ROUTE_ENV,
    AGENT_TESTING_ENV, RawAgentHookEnvelope,
};
use nmt_platform::windows::ipc::{MAX_MESSAGE_BYTES, send};

fn main() {
    let action = match std::env::args().nth(1).as_deref() {
        Some("codex") => "codex_hook",
        Some("claude") => "claude_hook",
        _ => return,
    };

    let (Ok(route), Ok(token), Some(version)) = (
        std::env::var(AGENT_ROUTE_ENV),
        std::env::var(AGENT_HOOK_TOKEN_ENV),
        std::env::var(AGENT_HOOK_VERSION_ENV)
            .ok()
            .and_then(|value| value.parse::<u32>().ok()),
    ) else {
        return;
    };

    let mut input = Vec::new();
    if std::io::stdin()
        .take(MAX_MESSAGE_BYTES as u64 + 1)
        .read_to_end(&mut input)
        .is_err()
        || input.len() > MAX_MESSAGE_BYTES
        || version != AGENT_HOOK_PROTOCOL_VERSION
    {
        return;
    }
    let Ok(payload) = serde_json::from_slice::<serde_json::Value>(&input) else {
        return;
    };
    let Ok(message) = serde_json::to_string(&RawAgentHookEnvelope {
        action: action.into(),
        version,
        token,
        route,
        payload,
    }) else {
        return;
    };
    if message.len() > MAX_MESSAGE_BYTES {
        return;
    }

    let testing = std::env::var(AGENT_TESTING_ENV).is_ok_and(|value| value == "1");
    let _ = send(&message, Duration::ZERO, testing);
}
