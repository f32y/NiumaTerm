//! Codex Hook adapter and user-config installer.
//!
//! The adapter runs before logging, config, primary election, session restore,
//! or GPUI initialization and always fails open. The installer edits only
//! NiumaTerm-owned entries in `~/.codex/hooks.json`, preserves unrelated
//! values, and never rewrites a file that fails to parse.

use std::io;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::{AGENT_HOOK_EXE_ENV, AgentEvent, AgentEventInput, AgentEventKind, HookInstallStatus};

/// Every Codex event that contributes to the pane lifecycle.
pub const HOOK_EVENTS: [&str; 6] = [
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PermissionRequest",
    "PostToolUse",
    "Stop",
];

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
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    Some(PathBuf::from(home).join(".codex").join("config.toml"))
}

/// `~/.codex/hooks.json`, the user-scope Codex Hook configuration.
pub fn hooks_path() -> Option<PathBuf> {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    Some(PathBuf::from(home).join(".codex").join("hooks.json"))
}

pub fn install_hooks(hooks_path: &Path) -> io::Result<()> {
    let command = current_hook_command()?;
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
    read_hooks(hooks_path).map_or(HookInstallStatus::NotInstalled, |settings| {
        status_of(&settings)
    })
}

fn current_hook_command() -> io::Result<String> {
    let executable = crate::agent_process()
        .hook_executable()
        .ok_or_else(|| invalid("NiumaTerm Hook executable path is unavailable"))?;
    if executable.contains('"') {
        return Err(invalid("NiumaTerm Hook executable path contains a quote"));
    }
    Ok(format!(r#""{executable}" codex"#))
}

/// Re-registering is idempotent: prior NiumaTerm entries are removed before
/// one current entry is appended to every lifecycle event.
fn install_into(settings: &mut Value, command: &str) -> io::Result<()> {
    uninstall_from(settings);
    let root = settings
        .as_object_mut()
        .expect("read_hooks only yields objects");
    let hooks = root.entry("hooks").or_insert_with(|| json!({}));
    let hooks = hooks
        .as_object_mut()
        .ok_or_else(|| invalid("existing \"hooks\" value is not an object"))?;
    for event in HOOK_EVENTS {
        let entries = hooks.entry(event).or_insert_with(|| json!([]));
        let entries = entries
            .as_array_mut()
            .ok_or_else(|| invalid("existing Codex hook event value is not an array"))?;
        entries.push(json!({
            "hooks": [{ "type": "command", "command": command, "timeout": 10 }]
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

fn status_of(settings: &Value) -> HookInstallStatus {
    let marked: Vec<_> = HOOK_EVENTS
        .iter()
        .flat_map(|event| event_commands(settings, event))
        .filter(|command| is_marked(command))
        .collect();
    if marked.is_empty() {
        HookInstallStatus::NotInstalled
    } else if HOOK_EVENTS
        .iter()
        .all(|event| event_commands(settings, event).any(is_marked))
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

fn read_hooks(hooks_path: &Path) -> io::Result<Value> {
    let text = match std::fs::read_to_string(hooks_path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(json!({})),
        Err(error) => return Err(error),
    };
    if text.trim().is_empty() {
        return Ok(json!({}));
    }
    let value: Value =
        serde_json::from_str(&text).map_err(|_| invalid("Codex hooks.json is not valid JSON"))?;
    if value.is_object() {
        Ok(value)
    } else {
        Err(invalid("Codex hooks.json root is not a JSON object"))
    }
}

fn write_hooks(hooks_path: &Path, settings: &Value) -> io::Result<()> {
    if let Some(parent) = hooks_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut text = serde_json::to_string_pretty(settings).map_err(io::Error::other)?;
    text.push('\n');
    let temp = hooks_path.with_extension("json.niumaterm-tmp");
    std::fs::write(&temp, text)?;
    std::fs::rename(&temp, hooks_path)
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
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
            let event = normalize(
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
        assert!(normalize(unknown, "route", "token", 1, "token").is_none());
        assert!(normalize(missing_turn, "route", "token", 1, "token").is_none());
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
        let event = normalize(
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

    const CURRENT_COMMAND: &str = r#""C:\Program Files\NiumaTerm\NiumaTermHook.exe" codex"#;
    const LEGACY_COMMAND: &str = r#"C:\Workspace\NiumaTerm\target\debug\NiumaTermHook.exe codex"#;

    fn user_hooks() -> Value {
        json!({
            "metadata": { "preserved": true },
            "hooks": {
                "PreToolUse": [{
                    "matcher": "Bash",
                    "hooks": [{
                        "type": "command",
                        "command": "rtk hook codex",
                        "statusMessage": "RTK rewrite",
                        "timeout": 30
                    }]
                }]
            }
        })
    }

    #[test]
    fn install_preserves_other_hooks_and_registers_every_event() {
        let mut settings = user_hooks();
        install_into(&mut settings, CURRENT_COMMAND).unwrap();

        assert_eq!(status_of(&settings), HookInstallStatus::Installed);
        assert_eq!(settings["metadata"]["preserved"], true);
        assert!(event_commands(&settings, "PreToolUse").any(|command| command == "rtk hook codex"));
        for event in HOOK_EVENTS {
            assert!(event_commands(&settings, event).any(|command| command == CURRENT_COMMAND));
        }
    }

    #[test]
    fn reinstall_migrates_legacy_entries_without_duplicates() {
        let mut settings = json!({});
        install_into(&mut settings, LEGACY_COMMAND).unwrap();
        assert_eq!(status_of(&settings), HookInstallStatus::Installed);

        install_into(&mut settings, CURRENT_COMMAND).unwrap();
        install_into(&mut settings, CURRENT_COMMAND).unwrap();

        assert_eq!(status_of(&settings), HookInstallStatus::Installed);
        for event in HOOK_EVENTS {
            assert_eq!(
                event_commands(&settings, event)
                    .filter(|command| is_marked(command))
                    .count(),
                1
            );
            assert!(event_commands(&settings, event).any(|command| command == CURRENT_COMMAND));
        }
    }

    #[test]
    fn uninstall_removes_only_niuma_entries_and_prunes_empty_groups() {
        let original = user_hooks();
        let mut settings = original.clone();
        install_into(&mut settings, CURRENT_COMMAND).unwrap();
        uninstall_from(&mut settings);

        assert_eq!(status_of(&settings), HookInstallStatus::NotInstalled);
        assert_eq!(settings, original);
    }

    #[test]
    fn missing_event_is_stale() {
        let mut settings = json!({});
        install_into(&mut settings, CURRENT_COMMAND).unwrap();
        settings["hooks"]
            .as_object_mut()
            .unwrap()
            .remove("PermissionRequest");
        assert_eq!(status_of(&settings), HookInstallStatus::Stale);
    }

    #[test]
    fn file_round_trip_is_atomic_and_invalid_json_is_kept() {
        let dir = std::env::temp_dir().join(format!("nmt-codex-hooks-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("hooks.json");
        std::fs::write(&path, serde_json::to_string(&user_hooks()).unwrap()).unwrap();

        install_hooks_with_command(&path, CURRENT_COMMAND).unwrap();
        assert_eq!(hooks_status(&path), HookInstallStatus::Installed);
        uninstall_hooks(&path).unwrap();
        assert_eq!(hooks_status(&path), HookInstallStatus::NotInstalled);
        assert_eq!(read_hooks(&path).unwrap(), user_hooks());

        let wrong_shape = r#"{"hooks":{"Stop":{}}}"#;
        std::fs::write(&path, wrong_shape).unwrap();
        assert!(install_hooks_with_command(&path, CURRENT_COMMAND).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), wrong_shape);

        std::fs::write(&path, "{ not valid").unwrap();
        assert!(install_hooks_with_command(&path, CURRENT_COMMAND).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ not valid");
        assert!(!path.with_extension("json.niumaterm-tmp").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn hooks_json_with_complete_niuma_registration_is_installed() {
        let dir = std::env::temp_dir().join(format!("nmt-codex-hooks-json-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("hooks.json");
        let mut hooks = serde_json::Map::new();
        for event in HOOK_EVENTS {
            hooks.insert(
                event.into(),
                serde_json::json!([{
                    "hooks": [{ "type": "command", "command": LEGACY_COMMAND, "timeout": 10 }]
                }]),
            );
        }
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&serde_json::json!({ "hooks": hooks })).unwrap(),
        )
        .unwrap();

        assert_eq!(hooks_status(&path), HookInstallStatus::Installed);

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
