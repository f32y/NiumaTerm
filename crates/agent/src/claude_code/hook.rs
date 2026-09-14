//! Claude Code integration: the stdin hook payload adapter and the installer
//! that manages NiumaTerm's hook registrations in `~/.claude/settings.json`.
//!
//! The adapter runs before logging, config, primary election, session restore,
//! or GPUI initialization and always fails open. Installer edits merge into
//! the user's existing settings: only entries whose command references the
//! NiumaTerm hook binary are ever touched, and a settings file that fails to
//! parse is never rewritten.

#[cfg(test)]
#[path = "hook_tests.rs"]
mod hook_tests;

use std::io;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::hook_store::{self, HookRegistration};
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

const REGISTRATION: HookRegistration = HookRegistration {
    events: &HOOK_EVENTS,
    file_label: "settings file",
    entry: |command| json!({"hooks": [{"type": "command", "command": command}]}),
};

pub fn install_hooks(settings_path: &Path) -> io::Result<()> {
    REGISTRATION.install(settings_path, HOOK_COMMAND)
}

pub fn uninstall_hooks(settings_path: &Path) -> io::Result<()> {
    REGISTRATION.uninstall(settings_path)
}

pub fn hooks_status(settings_path: &Path) -> HookInstallStatus {
    REGISTRATION.status(settings_path, Some(HOOK_COMMAND))
}
