//! Claude Code integration: the stdin hook payload adapter and the installer
//! that manages NiumaTerm's hook registrations in `~/.claude/settings.json`.
//!
//! The adapter runs before logging, config, primary election, session restore,
//! or GPUI initialization and always fails open. Installer edits merge into
//! the user's existing settings: only entries whose command references the
//! NiumaTerm hook binary are ever touched, and a settings file that fails to
//! parse is never rewritten.

use std::path::{Path, PathBuf};
use std::{env, io};

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
    let home = env::var_os("USERPROFILE").or_else(|| env::var_os("HOME"))?;

    Some(PathBuf::from(home).join(".claude").join("settings.json"))
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
mod tests {
    use std::{fs, process};

    use super::*;
    use crate::hook_store::event_commands;
    use crate::{AGENT_HOOK_PROTOCOL_VERSION, RawAgentHookEnvelope};

    #[test]
    fn claude_lifecycle_events_normalize_with_session_scoped_turns() {
        let cases = [
            ("SessionStart", AgentEventKind::SessionStarted, None),
            (
                "UserPromptSubmit",
                AgentEventKind::PromptSubmitted,
                Some("s"),
            ),
            ("PreToolUse", AgentEventKind::ToolStarted, Some("s")),
            (
                "Notification",
                AgentEventKind::PermissionRequested,
                Some("s"),
            ),
            ("PostToolUse", AgentEventKind::ToolFinished, Some("s")),
            ("Stop", AgentEventKind::Stopped, Some("s")),
        ];
        for (hook, kind, turn) in cases {
            let payload = json!({
                "session_id": "s",
                "transcript_path": "C:\\Users\\u\\.claude\\projects\\p\\s.jsonl",
                "cwd": "C:\\repo",
                "hook_event_name": hook,
            });
            let event = RawAgentHookEnvelope {
                action: "claude_hook".into(),
                version: AGENT_HOOK_PROTOCOL_VERSION,
                token: "test-token".into(),
                route: "test-route".into(),
                payload,
            }
            .into_event("test-token")
            .unwrap();
            assert_eq!(event.kind, kind);
            assert_eq!(event.agent, "claude");
            assert_eq!(event.turn_id.as_deref(), turn);
        }
    }

    #[test]
    fn claude_notification_message_becomes_body_and_subagent_stop_is_ignored() {
        let notification = json!({
            "session_id": "s",
            "hook_event_name": "Notification",
            "message": "Claude needs your permission to use Bash",
        });
        let event = normalize(notification, "route", "token", 1, "token").unwrap();
        assert_eq!(event.body, "Claude needs your permission to use Bash");
        let subagent_stop = json!({
            "session_id": "s",
            "hook_event_name": "SubagentStop",
        });
        assert!(normalize(subagent_stop, "route", "token", 1, "token").is_none());
    }

    const LEGACY_COMMAND: &str = "\"D:\\old install\\NiumaTermHook.exe\" claude";

    fn user_settings() -> Value {
        json!({
            "model": "opus",
            "hooks": {
                "PreToolUse": [{
                    "matcher": "Bash",
                    "hooks": [{ "type": "command", "command": "my-linter.exe" }]
                }]
            }
        })
    }

    #[test]
    fn install_registers_every_event_and_preserves_user_settings() {
        let mut settings = user_settings();
        install_into(&mut settings, HOOK_COMMAND).unwrap();
        assert_eq!(
            status_of(&settings, HOOK_COMMAND),
            HookInstallStatus::Installed
        );
        assert_eq!(settings["model"], "opus");
        assert!(event_commands(&settings, "PreToolUse").any(|command| command == "my-linter.exe"));
        for event in HOOK_EVENTS {
            assert!(event_commands(&settings, event).any(|command| command == HOOK_COMMAND));
        }
    }

    #[test]
    fn reinstall_migrates_legacy_absolute_path_without_duplicates() {
        let mut settings = json!({});
        install_into(&mut settings, LEGACY_COMMAND).unwrap();
        assert_eq!(status_of(&settings, HOOK_COMMAND), HookInstallStatus::Stale);
        install_into(&mut settings, HOOK_COMMAND).unwrap();
        assert_eq!(
            status_of(&settings, HOOK_COMMAND),
            HookInstallStatus::Installed
        );
        assert_eq!(event_commands(&settings, "Stop").count(), 1);
    }

    #[test]
    fn uninstall_removes_our_and_legacy_entries_and_prunes_empties() {
        let mut settings = user_settings();
        install_into(&mut settings, HOOK_COMMAND).unwrap();
        uninstall_from(&mut settings);
        assert_eq!(
            status_of(&settings, HOOK_COMMAND),
            HookInstallStatus::NotInstalled
        );
        assert_eq!(settings, user_settings());

        let mut legacy_only = json!({});
        install_into(&mut legacy_only, LEGACY_COMMAND).unwrap();
        uninstall_from(&mut legacy_only);
        assert_eq!(legacy_only, json!({}));
    }

    #[test]
    fn missing_events_read_as_stale() {
        let mut settings = json!({});
        install_into(&mut settings, HOOK_COMMAND).unwrap();
        settings["hooks"]
            .as_object_mut()
            .unwrap()
            .remove("Notification");
        assert_eq!(status_of(&settings, HOOK_COMMAND), HookInstallStatus::Stale);
    }

    #[test]
    fn malformed_shapes_are_refused_and_unparseable_files_are_kept() {
        let mut hooks_not_object = json!({ "hooks": [] });
        assert!(install_into(&mut hooks_not_object, HOOK_COMMAND).is_err());
        let mut event_not_array = json!({ "hooks": { "Stop": {} } });
        assert!(install_into(&mut event_not_array, HOOK_COMMAND).is_err());

        let dir = env::temp_dir().join(format!("nmt-installer-{}", process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        fs::write(&path, "{ broken").unwrap();
        assert!(install_hooks(&path).is_err());
        assert_eq!(hooks_status(&path), HookInstallStatus::NotInstalled);
        assert_eq!(fs::read_to_string(&path).unwrap(), "{ broken");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn file_round_trip_installs_and_uninstalls() {
        let dir = env::temp_dir().join(format!("nmt-installer-rt-{}", process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");

        assert_eq!(hooks_status(&path), HookInstallStatus::NotInstalled);
        install_hooks(&path).unwrap();
        assert_eq!(hooks_status(&path), HookInstallStatus::Installed);
        uninstall_hooks(&path).unwrap();
        assert_eq!(hooks_status(&path), HookInstallStatus::NotInstalled);
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(text.trim(), "{}");
        fs::remove_dir_all(&dir).unwrap();
    }
}
