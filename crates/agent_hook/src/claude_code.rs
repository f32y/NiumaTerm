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

use crate::{AGENT_HOOK_EXE_ENV, AgentEvent, AgentEventInput, AgentEventKind, HookInstallStatus};

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

/// Markers that identify hook entries owned by NiumaTerm: the current
/// env-var command and legacy installs that baked in an absolute path.
const HOOK_MARKERS: [&str; 2] = [AGENT_HOOK_EXE_ENV, "NiumaTermHook.exe"];

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
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
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

/// Re-registering is idempotent: prior NiumaTerm entries (including legacy
/// absolute-path installs) are removed before the current command is appended
/// to every event.
fn install_into(settings: &mut Value, command: &str) -> io::Result<()> {
    uninstall_from(settings);
    let root = settings
        .as_object_mut()
        .expect("read_settings only yields objects");
    let hooks = root.entry("hooks").or_insert_with(|| json!({}));
    let hooks = hooks
        .as_object_mut()
        .ok_or_else(|| invalid("existing \"hooks\" value is not an object"))?;
    for event in HOOK_EVENTS {
        let entries = hooks.entry(event).or_insert_with(|| json!([]));
        let entries = entries
            .as_array_mut()
            .ok_or_else(|| invalid("existing hook event value is not an array"))?;
        entries.push(json!({
            "hooks": [{ "type": "command", "command": command }]
        }));
    }
    Ok(())
}

fn uninstall_from(settings: &mut Value) {
    let Some(hooks) = settings.get_mut("hooks").and_then(Value::as_object_mut) else {
        return;
    };
    for entries in hooks.values_mut() {
        let Some(groups) = entries.as_array_mut() else {
            continue;
        };
        for group in groups.iter_mut() {
            if let Some(commands) = group.get_mut("hooks").and_then(Value::as_array_mut) {
                commands.retain(|hook| !is_niuma_hook(hook));
            }
        }
        // A matcher group with no commands does nothing; pruning it keeps the
        // file as clean as before the install.
        groups.retain(|group| {
            group
                .get("hooks")
                .and_then(Value::as_array)
                .is_none_or(|commands| !commands.is_empty())
        });
    }
    hooks.retain(|_, entries| entries.as_array().is_none_or(|groups| !groups.is_empty()));
    if hooks.is_empty() {
        settings
            .as_object_mut()
            .expect("checked above")
            .remove("hooks");
    }
}

fn status_of(settings: &Value, command: &str) -> HookInstallStatus {
    let marked: Vec<&str> = HOOK_EVENTS
        .iter()
        .flat_map(|event| event_commands(settings, event))
        .filter(|value| is_marked(value))
        .collect();
    if marked.is_empty() {
        HookInstallStatus::NotInstalled
    } else if marked.iter().all(|value| *value == command)
        && HOOK_EVENTS
            .iter()
            .all(|event| event_commands(settings, event).any(|value| value == command))
    {
        HookInstallStatus::Installed
    } else {
        HookInstallStatus::Stale
    }
}

fn event_commands<'a>(settings: &'a Value, event: &str) -> impl Iterator<Item = &'a str> {
    settings
        .pointer(&format!("/hooks/{event}"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|group| group.get("hooks").and_then(Value::as_array))
        .flatten()
        .filter_map(|hook| hook.get("command").and_then(Value::as_str))
}

fn is_niuma_hook(hook: &Value) -> bool {
    hook.get("command")
        .and_then(Value::as_str)
        .is_some_and(is_marked)
}

fn is_marked(command: &str) -> bool {
    HOOK_MARKERS.iter().any(|marker| command.contains(marker))
}

/// A missing or empty file reads as an empty object; anything unparseable is
/// surfaced as an error so a broken settings file is never overwritten.
fn read_settings(settings_path: &Path) -> io::Result<Value> {
    let text = match std::fs::read_to_string(settings_path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(json!({})),
        Err(error) => return Err(error),
    };
    if text.trim().is_empty() {
        return Ok(json!({}));
    }
    let value: Value =
        serde_json::from_str(&text).map_err(|_| invalid("settings file is not valid JSON"))?;
    if value.is_object() {
        Ok(value)
    } else {
        Err(invalid("settings file root is not a JSON object"))
    }
}

/// Write-then-rename so a crash mid-write cannot truncate the user's settings.
fn write_settings(settings_path: &Path, settings: &Value) -> io::Result<()> {
    if let Some(parent) = settings_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut text = serde_json::to_string_pretty(settings).map_err(io::Error::other)?;
    text.push('\n');
    let temp = settings_path.with_extension("json.niumaterm-tmp");
    std::fs::write(&temp, text)?;
    std::fs::rename(&temp, settings_path)
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;
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
            let payload = serde_json::json!({
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
        let notification = serde_json::json!({
            "session_id": "s",
            "hook_event_name": "Notification",
            "message": "Claude needs your permission to use Bash",
        });
        let event = normalize(notification, "route", "token", 1, "token").unwrap();
        assert_eq!(event.body, "Claude needs your permission to use Bash");
        let subagent_stop = serde_json::json!({
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

        let dir = std::env::temp_dir().join(format!("nmt-installer-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        std::fs::write(&path, "{ broken").unwrap();
        assert!(install_hooks(&path).is_err());
        assert_eq!(hooks_status(&path), HookInstallStatus::NotInstalled);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ broken");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn file_round_trip_installs_and_uninstalls() {
        let dir = std::env::temp_dir().join(format!("nmt-installer-rt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");

        assert_eq!(hooks_status(&path), HookInstallStatus::NotInstalled);
        install_hooks(&path).unwrap();
        assert_eq!(hooks_status(&path), HookInstallStatus::Installed);
        uninstall_hooks(&path).unwrap();
        assert_eq!(hooks_status(&path), HookInstallStatus::NotInstalled);
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.trim(), "{}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
