use std::{env, fs, process};

use serde_json::{Map, from_str, to_string, to_string_pretty};

use crate::AGENT_HOOK_PROTOCOL_VERSION;
use crate::codex::hook::*;

fn fixture_events() -> Vec<Value> {
    let fixture: Value =
        from_str(include_str!("../../../tests/fixtures/codex-0.144.1.json")).unwrap();
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

const CURRENT_COMMAND: &str = r"C:\Soft\NiumaTerm\NmtAgentHook.exe codex";
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
