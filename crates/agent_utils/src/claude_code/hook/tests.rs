use std::{env, fs, process};

use crate::claude_code::hook::*;
use crate::hook_store::event_commands;
use crate::{AGENT_HOOK_PROTOCOL_VERSION, RawAgentHookMessage};

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
        let event = RawAgentHookMessage {
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
