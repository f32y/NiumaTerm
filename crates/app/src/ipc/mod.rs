//! Application message parsing and dispatch for the platform IPC transport.
//!
//! The Windows mutex and Named Pipe implementation live in `nmt_platform`.

use std::str;

use futures::channel::mpsc::UnboundedSender;
use nmt_agent_utils::{AgentEvent, RawAgentHookMessage, agent_process};
use nmt_platform::windows::ipc::{MAX_MESSAGE_BYTES, spawn_server};
use serde_json::{Value, from_str, from_value};
use tracing::warn;

use crate::cli;
use crate::cli::CliAction;

pub(crate) enum IpcAction {
    Cli(CliAction),
    Agent(AgentEvent),
}

/// Run the primary's pipe server on a dedicated thread: accept one client at
/// a time, read its line(s), parse, and send each action into `tx`. Runs for
/// the process lifetime.
pub(crate) fn spawn_pipe_server(tx: UnboundedSender<IpcAction>, testing: bool) {
    spawn_server(testing, move |bytes| {
        match parse_message(&bytes, agent_process().hook_token()) {
            Ok(action) => return tx.unbounded_send(action).is_ok(),
            Err(error) => warn!("ignoring IPC message: {error}"),
        }
        true
    });
}

fn parse_message(bytes: &[u8], expected_token: &str) -> Result<IpcAction, String> {
    if bytes.len() > MAX_MESSAGE_BYTES {
        return Err("message exceeds 64 KiB".into());
    }

    let text = str::from_utf8(bytes).map_err(|_| "message is not UTF-8")?;
    let text = text.trim_end_matches(['\r', '\n', ' ', '\t']);

    if text.is_empty() {
        return Err("empty message".into());
    }

    if text.contains(['\r', '\n']) {
        return Err("connection contains more than one message".into());
    }

    if text.starts_with('{') {
        let value: Value = from_str(text).map_err(|_| "invalid agent message")?;

        match value.get("action").and_then(Value::as_str) {
            Some("codex_hook" | "claude_hook") => from_value::<RawAgentHookMessage>(value)
                .map_err(|_| "invalid agent hook message")?
                .into_event(expected_token)
                .map(IpcAction::Agent)
                .ok_or_else(|| "invalid agent Hook fields".into()),
            _ => Err("unsupported IPC action".into()),
        }
    } else {
        cli::parse_nmt_url(text)
            .map(IpcAction::Cli)
            .map_err(|_| "invalid nmt URL".into())
    }
}

#[cfg(test)]
mod tests;
