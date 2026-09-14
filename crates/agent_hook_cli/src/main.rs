//! The hook helper Claude Code and Codex run on every hook event.
//!
//! It reads one JSON payload from stdin and forwards it to the NiumaTerm
//! instance named by the environment, over the same named pipe the single-
//! instance handoff uses. This helper currently delivers events on Windows;
//! the inert entry point keeps other workspace builds available.

#[cfg(windows)]
use std::env;
#[cfg(windows)]
use std::io::{self, Read};
#[cfg(windows)]
use std::time::Duration;

#[cfg(windows)]
use nmt_agent::{
    AGENT_HOOK_PROTOCOL_VERSION, AGENT_HOOK_TOKEN_ENV, AGENT_HOOK_VERSION_ENV, AGENT_ROUTE_ENV,
    AGENT_TESTING_ENV, RawAgentHookMessage,
};
#[cfg(windows)]
use nmt_platform::windows::ipc::{MAX_MESSAGE_BYTES, send};
#[cfg(windows)]
use serde_json::{Value, from_slice, to_string};

#[cfg(windows)]
fn main() {
    let action = match env::args().nth(1).as_deref() {
        Some("codex") => "codex_hook",
        Some("claude") => "claude_hook",
        _ => return,
    };

    let (Ok(route), Ok(token), Ok(version)) = (
        env::var(AGENT_ROUTE_ENV),
        env::var(AGENT_HOOK_TOKEN_ENV),
        env::var(AGENT_HOOK_VERSION_ENV),
    ) else {
        return;
    };

    let Ok(version) = version.parse::<u32>() else {
        eprintln!("NiumaTerm hook protocol version is invalid");

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
        eprintln!("NiumaTerm hook input is unreadable, too large, or uses an unsupported version");

        return;
    }

    let Ok(payload) = from_slice::<Value>(&input) else {
        eprintln!("NiumaTerm hook input is not valid JSON");

        return;
    };

    let Ok(message) = to_string(&RawAgentHookMessage {
        action: action.into(),
        version,
        token,
        route,
        payload,
    }) else {
        eprintln!("NiumaTerm hook message could not be encoded");

        return;
    };

    if message.len() > MAX_MESSAGE_BYTES {
        eprintln!("NiumaTerm hook message exceeds the IPC size limit");

        return;
    }

    let testing = env::var(AGENT_TESTING_ENV).is_ok_and(|value| value == "1");

    if send(&message, Duration::from_millis(200), testing).is_err() {
        eprintln!("NiumaTerm hook delivery failed");
    }
}

#[cfg(not(windows))]
fn main() {}
