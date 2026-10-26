use std::fs;
use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};
use tempfile::{TempDir, tempdir};

use crate::chat::{Event, Item, QuestionAnswer, QuestionMode, QuestionResponse};
use crate::claude_code::stream_json::Session;
use crate::{AgentWorkspace, LaunchConfig};

fn session() -> (Session, TempDir) {
    let directory = tempdir().unwrap();

    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude/capture-stream.ps1");

    let launch = LaunchConfig {
        executable: "powershell.exe".into(),
        executable_args: vec![
            "-NoLogo".into(),
            "-NoProfile".into(),
            "-NonInteractive".into(),
            "-File".into(),
            fixture.to_string_lossy().into_owned(),
        ],
        env: vec![
            (
                "NMT_FAKE_STREAM_LOG".into(),
                directory
                    .path()
                    .join("input.jsonl")
                    .to_string_lossy()
                    .into_owned(),
            ),
            ("PSExecutionPolicyPreference".into(), "Bypass".into()),
        ],
        ..LaunchConfig::default()
    };

    let session =
        Session::spawn(&launch, &AgentWorkspace::default(), None, |_| {}, |_| {}).unwrap();

    (session, directory)
}

fn recorded(session: &mut Session, directory: &TempDir) -> Vec<Value> {
    nmt_platform::runtime()
        .block_on(session.shutdown(Duration::from_secs(5), false))
        .unwrap();

    fs::read_to_string(directory.path().join("input.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn request(id: &str, kind: &str, payload: Value) -> Value {
    json!({
        "type": "control_request", "request_id": id,
        "request": {"subtype": "request_user_dialog", "dialog_kind": kind, "payload": payload}
    })
}

fn refusal(id: &str) -> Value {
    request(
        id,
        "refusal_fallback_prompt",
        json!({
            "originalModel": "original", "fallbackModel": "fallback",
            "apiRefusalCategory": null, "retractedMessageUuids": ["refused"]
        }),
    )
}

fn answer(label: &str) -> Option<Vec<QuestionAnswer>> {
    Some(vec![QuestionAnswer {
        selected: vec![label.into()],
        custom: None,
    }])
}

#[test]
fn fallback_waits_for_user_and_edit_returns_prompt_and_removes_refused_output() {
    let (mut session, directory) = session();

    session.process(json!({"type": "user", "uuid": "user", "message": {
        "content": [{"type": "text", "text": "Original question"}]
    }}));

    session.process(json!({"type": "assistant", "uuid": "refused", "message": {
        "content": [{"type": "text", "text": "Partial refused response"}]
    }}));

    let events = session.process(refusal("dialog"));

    assert!(matches!(&events[..], [Event::InputRequested(question)]
        if question.id == "dialog" && question.mode == QuestionMode::Dialog));
    assert!(session.control.dialogs.has_pending());
    assert!(session.process(refusal("dialog")).is_empty());
    assert!(session.respond_input("dialog", answer("unknown")).is_err());

    let reply = session
        .respond_input("dialog", answer("Edit the prompt and retry"))
        .unwrap();

    assert_eq!(reply.response, QuestionResponse::Settled);
    assert_eq!(reply.edit_prompt.as_deref(), Some("Original question"));
    assert!(
        matches!(&reply.events[..], [Event::ItemsRemoved { item_ids }] if !item_ids.is_empty())
    );
    assert!(!session.control.dialogs.has_pending());
    assert!(session.respond_input("dialog", None).is_err());

    let messages = recorded(&mut session, &directory);

    let kinds = messages[0]["request"]["supportedDialogKinds"]
        .as_array()
        .unwrap();

    assert_eq!(kinds.len(), 5);
    assert!(kinds.contains(&json!("refusal_fallback_prompt")));
    assert!(!kinds.contains(&json!("enum_prompt")));

    let replies: Vec<_> = messages
        .iter()
        .filter(|message| message["type"] == "control_response")
        .collect();

    assert_eq!(replies.len(), 1);
    assert_eq!(
        replies[0]["response"],
        json!({
            "subtype": "success", "request_id": "dialog",
            "response": {"behavior": "completed", "result": "edit_prompt"}
        })
    );
    assert!(!messages.iter().any(|message| message["type"] == "user"));
}

#[test]
fn unsupported_and_malformed_dialogs_report_errors_without_user_cancellation() {
    let (mut session, directory) = session();

    for message in [
        request("unknown", "enum_prompt", json!({})),
        request("invalid", "refusal_fallback_prompt", json!({})),
    ] {
        assert!(matches!(
            &session.process(message)[..],
            [Event::Error { fatal: false, .. }]
        ));
    }

    assert!(!session.control.dialogs.has_pending());

    let messages = recorded(&mut session, &directory);

    let replies: Vec<_> = messages
        .iter()
        .filter(|message| message["type"] == "control_response")
        .collect();

    assert_eq!(replies.len(), 2);

    for reply in replies {
        assert_eq!(reply["response"]["subtype"], "error");
        assert!(reply["response"]["response"].is_null());
    }
}

#[test]
fn form_decline_cancel_and_accept_send_distinct_responses() {
    let (mut session, directory) = session();

    for id in ["decline", "cancel", "accept"] {
        let events = session.process(json!({
            "type": "control_request", "request_id": id,
            "request": {
                "subtype": "elicitation", "mcp_server_name": "demo", "message": "Your name",
                "requested_schema": {"type": "object", "required": ["name"], "properties": {
                    "name": {"type": "string", "minLength": 1}
                }}
            }
        }));

        assert!(
            matches!(&events[..], [Event::InputRequested(question)] if question.mode == QuestionMode::Form)
        );
    }

    session.decline_input("decline").unwrap();
    session.respond_input("cancel", None).unwrap();

    session
        .respond_input(
            "accept",
            Some(vec![QuestionAnswer {
                selected: vec![],
                custom: Some("Ada".into()),
            }]),
        )
        .unwrap();

    let messages = recorded(&mut session, &directory);

    let replies: Vec<_> = messages
        .iter()
        .filter(|message| message["type"] == "control_response")
        .map(|message| message["response"]["response"].clone())
        .collect();

    assert_eq!(
        replies,
        vec![
            json!({"action": "decline"}),
            json!({"action": "cancel"}),
            json!({"action": "accept", "content": {"name": "Ada"}})
        ]
    );
}

#[test]
fn fallback_notices_update_session_model_only_for_session_changes() {
    let (mut session, _directory) = session();

    session.applied_model = Some("original".into());

    let local = session.process(json!({
        "type": "system", "subtype": "model_fallback", "scope": "local",
        "original_model": "original", "fallback_model": "local", "uuid": "local"
    }));

    assert_eq!(session.applied_model.as_deref(), Some("original"));
    assert!(
        !local
            .iter()
            .any(|event| matches!(event, Event::ModelSelection { .. }))
    );
    assert!(
        local
            .iter()
            .any(|event| matches!(event, Event::ItemCompleted(Item::Other { .. })))
    );

    for (subtype, direction, expected) in [
        ("model_refusal_fallback", "fallback", "fallback"),
        ("model_fallback", "revert", "original"),
        ("model_consent_fallback", "fallback", "fallback"),
    ] {
        let events = session.process(json!({
            "type": "system", "subtype": subtype, "scope": "session", "direction": direction,
            "original_model": "original", "fallback_model": "fallback", "uuid": direction
        }));

        assert_eq!(session.applied_model.as_deref(), Some(expected));
        assert!(events.iter().any(|event| matches!(event,
            Event::ModelSelection { model: Some(model), .. } if model == expected)));
        assert!(
            events
                .iter()
                .any(|event| matches!(event, Event::ItemCompleted(Item::Other { .. })))
        );
    }
}
