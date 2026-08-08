#![cfg(target_os = "windows")]

use std::env;
use std::io::{self, Read};
use std::time::Duration;

use nmt_agent_utils::{
    AGENT_HOOK_PROTOCOL_VERSION, AGENT_HOOK_TOKEN_ENV, AGENT_HOOK_VERSION_ENV, AGENT_ROUTE_ENV,
    AGENT_TESTING_ENV, RawAgentHookMessage,
};
use nmt_platform::windows::ipc::{MAX_MESSAGE_BYTES, send};
use serde_json::{Value, from_slice, to_string};

fn main() {
    let action = match env::args().nth(1).as_deref() {
        Some("codex") => "codex_hook",
        Some("claude") => "claude_hook",
        _ => return,
    };

    let (Ok(route), Ok(token), Some(version)) = (
        env::var(AGENT_ROUTE_ENV),
        env::var(AGENT_HOOK_TOKEN_ENV),
        env::var(AGENT_HOOK_VERSION_ENV)
            .ok()
            .and_then(|value| value.parse::<u32>().ok()),
    ) else {
        return;
    };

    let mut input = Vec::new();
    if io::stdin()
        .take(MAX_MESSAGE_BYTES as u64 + 1)
        .read_to_end(&mut input)
        .is_err()
        || input.len() > MAX_MESSAGE_BYTES
        || version != AGENT_HOOK_PROTOCOL_VERSION
    {
        return;
    }

    let Ok(payload) = from_slice::<Value>(&input) else {
        return;
    };

    let Ok(message) = to_string(&RawAgentHookMessage {
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

    let testing = env::var(AGENT_TESTING_ENV).is_ok_and(|value| value == "1");

    let _ = send(&message, Duration::ZERO, testing);
}
