//! Codex Hook adapter and user-config installer.
//!
//! The adapter runs before logging, config, primary election, session restore,
//! or GPUI initialization and always fails open. The installer edits only
//! NiumaTerm-owned entries in `~/.codex/hooks.json`, preserves unrelated
//! values, and never rewrites a file that fails to parse.

use std::path::{Path, PathBuf};
use std::{env, io};

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
    let home = env::var_os("USERPROFILE").or_else(|| env::var_os("HOME"))?;

    Some(PathBuf::from(home).join(".codex").join("config.toml"))
}

/// `~/.codex/hooks.json`, the user-scope Codex Hook configuration.
pub fn hooks_path() -> Option<PathBuf> {
    let home = env::var_os("USERPROFILE").or_else(|| env::var_os("HOME"))?;

    Some(PathBuf::from(home).join(".codex").join("hooks.json"))
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
mod tests {
    use std::{fs, process};

    use serde_json::{Map, from_str, to_string, to_string_pretty};

    use super::*;
    use crate::AGENT_HOOK_PROTOCOL_VERSION;

    fn fixture_events() -> Vec<Value> {
        let fixture: Value =
            from_str(include_str!("../../tests/fixtures/codex-0.144.1.json")).unwrap();
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
        let unknown = json!({"hook_event_name":"Other","session_id":"s"});
        let missing_turn = json!({"hook_event_name":"Stop","session_id":"s"});
        assert!(normalize(unknown, "route", "token", 1, "token").is_none());
        assert!(normalize(missing_turn, "route", "token", 1, "token").is_none());
    }

    #[test]
    fn unknown_fields_and_unicode_presentation_are_safe() {
        let payload = json!({
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

    const CURRENT_COMMAND: &str = r"C:\Soft\NiumaTerm\NiumaTermHook.exe codex";
    const LEGACY_COMMAND: &str = r#""C:\Program Files\NiumaTerm\NiumaTermHook.exe" codex"#;

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

        assert_eq!(
            status_of(&settings, CURRENT_COMMAND),
            HookInstallStatus::Installed
        );
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
        assert_eq!(
            status_of(&settings, LEGACY_COMMAND),
            HookInstallStatus::Installed
        );

        install_into(&mut settings, CURRENT_COMMAND).unwrap();
        install_into(&mut settings, CURRENT_COMMAND).unwrap();

        assert_eq!(
            status_of(&settings, CURRENT_COMMAND),
            HookInstallStatus::Installed
        );
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

        assert_eq!(
            status_of(&settings, CURRENT_COMMAND),
            HookInstallStatus::NotInstalled
        );
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
        assert_eq!(
            status_of(&settings, CURRENT_COMMAND),
            HookInstallStatus::Stale
        );
    }

    #[test]
    fn file_round_trip_is_atomic_and_invalid_json_is_kept() {
        let dir = env::temp_dir().join(format!("nmt-codex-hooks-{}", process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("hooks.json");
        fs::write(&path, to_string(&user_hooks()).unwrap()).unwrap();

        install_hooks_with_command(&path, CURRENT_COMMAND).unwrap();
        assert_eq!(
            status_of(&read_hooks(&path).unwrap(), CURRENT_COMMAND),
            HookInstallStatus::Installed
        );
        uninstall_hooks(&path).unwrap();
        assert_eq!(
            status_of(&read_hooks(&path).unwrap(), CURRENT_COMMAND),
            HookInstallStatus::NotInstalled
        );
        assert_eq!(read_hooks(&path).unwrap(), user_hooks());

        let wrong_shape = r#"{"hooks":{"Stop":{}}}"#;
        fs::write(&path, wrong_shape).unwrap();
        assert!(install_hooks_with_command(&path, CURRENT_COMMAND).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), wrong_shape);

        fs::write(&path, "{ not valid").unwrap();
        assert!(install_hooks_with_command(&path, CURRENT_COMMAND).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "{ not valid");
        assert!(!path.with_extension("json.niumaterm-tmp").exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn complete_registration_with_an_old_command_is_stale() {
        let dir = env::temp_dir().join(format!("nmt-codex-hooks-json-{}", process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("hooks.json");
        let mut hooks = Map::new();
        for event in HOOK_EVENTS {
            hooks.insert(
                event.into(),
                json!([{
                    "hooks": [{ "type": "command", "command": LEGACY_COMMAND, "timeout": 10 }]
                }]),
            );
        }
        fs::write(&path, to_string_pretty(&json!({ "hooks": hooks })).unwrap()).unwrap();

        assert_eq!(
            status_of(&read_hooks(&path).unwrap(), CURRENT_COMMAND),
            HookInstallStatus::Stale
        );

        fs::remove_dir_all(&dir).unwrap();
    }
}
