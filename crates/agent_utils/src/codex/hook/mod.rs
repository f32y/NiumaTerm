//! Codex Hook adapter and user-config installer.
//!
//! The adapter runs before logging, config, primary election, session restore,
//! or GPUI initialization and always fails open. The installer edits only
//! NiumaTerm-owned entries in `~/.codex/hooks.json`, preserves unrelated
//! values, and never rewrites a file that fails to parse.

use std::io;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::hook_store::{self, event_commands, is_marked, uninstall_from};
use crate::{
    AgentEvent, AgentEventInput, AgentEventKind, HookInstallStatus, agent_process,
    build_windows_hook_command,
};

/// Every Codex event that contributes to the pane lifecycle.
pub const HOOK_EVENTS: [&str; 6] = [
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PermissionRequest",
    "PostToolUse",
    "Stop",
];

pub(crate) fn normalize(
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

/// `~/.codex/config.toml`, the user-scope Codex configuration.
pub fn config_path() -> Option<PathBuf> {
    Some(hook_store::home_dir()?.join(".codex").join("config.toml"))
}

/// `~/.codex/hooks.json`, the user-scope Codex Hook configuration.
pub fn hooks_path() -> Option<PathBuf> {
    Some(hook_store::home_dir()?.join(".codex").join("hooks.json"))
}

pub fn install_hooks(hooks_path: &Path) -> io::Result<()> {
    let command = hook_command()?;

    install_hooks_with_command(hooks_path, &command)
}

fn install_hooks_with_command(hooks_path: &Path, command: &str) -> io::Result<()> {
    let mut settings = read_hooks(hooks_path)?;

    install_into(&mut settings, command)?;

    write_hooks(hooks_path, &settings)
}

pub fn uninstall_hooks(hooks_path: &Path) -> io::Result<()> {
    if !hooks_path.exists() {
        return Ok(());
    }

    let mut settings = read_hooks(hooks_path)?;

    uninstall_from(&mut settings);

    write_hooks(hooks_path, &settings)
}

pub fn hooks_status(hooks_path: &Path) -> HookInstallStatus {
    let Ok(settings) = read_hooks(hooks_path) else {
        return HookInstallStatus::NotInstalled;
    };

    match hook_command() {
        Ok(command) => status_of(&settings, &command),
        Err(_)
            if HOOK_EVENTS
                .iter()
                .flat_map(|event| event_commands(&settings, event))
                .any(is_marked) =>
        {
            HookInstallStatus::Stale
        }
        Err(_) => HookInstallStatus::NotInstalled,
    }
}

/// The exact command written to Codex and used when checking whether an
/// existing registration still matches this NiumaTerm installation.
pub fn hook_command() -> io::Result<String> {
    let executable = agent_process()
        .hook_executable()
        .ok_or_else(|| invalid("NiumaTerm Hook executable path is unavailable"))?;

    build_windows_hook_command(executable, "codex")
}

/// Binds the shared hook store to Codex's event list and entry shape. The
/// per-entry timeout keeps a hung hook from stalling the Codex turn.
fn install_into(settings: &mut Value, command: &str) -> io::Result<()> {
    hook_store::install_into(settings, &HOOK_EVENTS, command, |command| {
        json!({
            "hooks": [{ "type": "command", "command": command, "timeout": 10 }]
        })
    })
}

fn status_of(settings: &Value, command: &str) -> HookInstallStatus {
    hook_store::status_of(settings, &HOOK_EVENTS, command)
}

fn read_hooks(hooks_path: &Path) -> io::Result<Value> {
    hook_store::read(hooks_path, "Codex hooks.json")
}

fn write_hooks(hooks_path: &Path, settings: &Value) -> io::Result<()> {
    hook_store::write(hooks_path, settings)
}

fn invalid(message: &str) -> io::Error {
    hook_store::invalid(message)
}

#[cfg(test)]
mod tests;
