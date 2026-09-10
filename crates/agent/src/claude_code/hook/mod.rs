//! Claude Code integration: the stdin hook payload adapter and the installer
//! that manages NiumaTerm's hook registrations in `~/.claude/settings.json`.
//!
//! The adapter runs before logging, config, primary election, session restore,
//! or GPUI initialization and always fails open. Installer edits merge into
//! the user's existing settings: only entries whose command references the
//! NiumaTerm hook binary are ever touched, and a settings file that fails to
//! parse is never rewritten.

use std::io;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::hook_store::{self, uninstall_from};
use crate::{AgentEvent, AgentEventInput, AgentEventKind, HookInstallStatus};

/// Every hook event the adapter normalizes. Keep in sync with the `normalize`
/// match below.
pub const HOOK_EVENTS: [&str; 6] = [
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "Notification",
    "Stop",
];

/// The registered hook command. Claude Code executes hook commands through
/// Git Bash even on Windows, so POSIX expansion of the pane-injected
/// `NMT_AGENT_HOOK_EXE` locates the binary without baking an install path
/// into the user's settings, and the guard exits 0 in Claude Code sessions
/// outside NiumaTerm instead of logging a command-not-found error.
pub const HOOK_COMMAND: &str =
    r#"if [ -n "$NMT_AGENT_HOOK_EXE" ]; then "$NMT_AGENT_HOOK_EXE" claude; fi"#;

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
        // Claude Code surfaces both permission prompts and idle-input prompts
        // through the Notification hook; both mean the pane needs attention.
        "Notification" => (
            AgentEventKind::PermissionRequested,
            "Claude Code needs input",
            payload
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Claude Code is waiting for input"),
        ),
        "PostToolUse" => (AgentEventKind::ToolFinished, "", ""),
        // SubagentStop is deliberately ignored: the parent turn is still
        // running when a subagent finishes.
        "Stop" => (
            AgentEventKind::Stopped,
            "Claude Code finished",
            "Claude Code completed the turn",
        ),
        _ => return None,
    };

    // Claude Code hook payloads carry no turn identifier, so the session is
    // the finest ownership granularity available. Replays across turns are
    // harmless: PromptSubmitted resets the pane state either way.
    let turn_id = (kind != AgentEventKind::SessionStarted).then_some(session_id);

    AgentEvent::validate(
        AgentEventInput {
            route,
            token,
            version,
            agent: "claude",
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

/// `~/.claude/settings.json`, the user-scope Claude Code configuration.
pub fn settings_path() -> Option<PathBuf> {
    Some(
        hook_store::home_dir()?
            .join(".claude")
            .join("settings.json"),
    )
}

pub fn install_hooks(settings_path: &Path) -> io::Result<()> {
    let mut settings = read_settings(settings_path)?;

    install_into(&mut settings, HOOK_COMMAND)?;

    write_settings(settings_path, &settings)
}

pub fn uninstall_hooks(settings_path: &Path) -> io::Result<()> {
    if !settings_path.exists() {
        return Ok(());
    }

    let mut settings = read_settings(settings_path)?;

    uninstall_from(&mut settings);

    write_settings(settings_path, &settings)
}

pub fn hooks_status(settings_path: &Path) -> HookInstallStatus {
    match read_settings(settings_path) {
        Ok(settings) => status_of(&settings, HOOK_COMMAND),
        Err(_) => HookInstallStatus::NotInstalled,
    }
}

/// Binds the shared hook store to Claude Code's event list and entry shape.
fn install_into(settings: &mut Value, command: &str) -> io::Result<()> {
    hook_store::install_into(settings, &HOOK_EVENTS, command, |command| {
        json!({
            "hooks": [{ "type": "command", "command": command }]
        })
    })
}

fn status_of(settings: &Value, command: &str) -> HookInstallStatus {
    hook_store::status_of(settings, &HOOK_EVENTS, command)
}

fn read_settings(settings_path: &Path) -> io::Result<Value> {
    hook_store::read(settings_path, "settings file")
}

fn write_settings(settings_path: &Path, settings: &Value) -> io::Result<()> {
    hook_store::write(settings_path, settings)
}

#[cfg(test)]
mod tests;
