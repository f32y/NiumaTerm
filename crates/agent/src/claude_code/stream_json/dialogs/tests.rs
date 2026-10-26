use std::slice;

use serde_json::{Value, json};

use crate::chat::{Event, QuestionAnswer, QuestionMode, QuestionRequest, QuestionResolution};
use crate::claude_code::stream_json::control::ControlState;
use crate::claude_code::stream_json::dialogs::Dialogs;

fn refusal_request() -> Value {
    json!({
        "subtype": "request_user_dialog",
        "dialog_kind": "refusal_fallback_prompt",
        "payload": {
            "originalModel": "claude-opus-5-5",
            "fallbackModel": "claude-opus-5",
            "retractedMessageUuids": ["partial-response", "partial-reasoning"]
        }
    })
}

fn usage_request() -> Value {
    json!({
        "subtype": "request_user_dialog",
        "dialog_kind": "fable_overage_consent_prompt",
        "payload": {"overagesEnabled": true}
    })
}

fn form_request() -> Value {
    json!({
        "subtype": "elicitation",
        "mcp_server_name": "release-tools",
        "message": "Choose a release name.",
        "requested_schema": {
            "type": "object",
            "properties": {"name": {"type": "string", "minLength": 1}},
            "required": ["name"]
        }
    })
}

fn url_request() -> Value {
    json!({
        "subtype": "elicitation",
        "mode": "url",
        "mcp_server_name": "account-tools",
        "message": "Authorize access to your account.",
        "url": "https://example.com/authorize",
        "elicitation_id": "account-authorization"
    })
}

fn select(request: &QuestionRequest, index: usize) -> QuestionAnswer {
    QuestionAnswer {
        selected: vec![request.questions[0].options[index].label.clone()],
        custom: None,
    }
}

#[test]
fn duplicate_pending_and_retired_ids_do_not_create_another_request() {
    let mut dialogs = Dialogs::default();

    let request = dialogs.receive("refusal", &refusal_request()).unwrap();

    assert!(request.is_some());
    assert!(
        dialogs
            .receive("refusal", &refusal_request())
            .unwrap()
            .is_none()
    );
    assert!(dialogs.receive("refusal", &json!({})).unwrap().is_none());
    assert!(dialogs.contains("refusal"));

    dialogs.retire("refusal").unwrap();

    assert!(!dialogs.contains("refusal"));
    assert!(!dialogs.has_pending());
    assert!(
        dialogs
            .receive("refusal", &refusal_request())
            .unwrap()
            .is_none()
    );
    assert!(
        dialogs
            .receive("refusal", &url_request())
            .unwrap()
            .is_none()
    );
    assert!(dialogs.respond("refusal", None).is_err());
}

#[test]
fn different_dialog_kinds_remain_independently_pending() {
    let mut dialogs = Dialogs::default();

    for (id, payload, mode) in [
        ("refusal", refusal_request(), QuestionMode::Dialog),
        ("credits", usage_request(), QuestionMode::Dialog),
        ("form", form_request(), QuestionMode::Form),
        ("url", url_request(), QuestionMode::Form),
    ] {
        let request = dialogs.receive(id, &payload).unwrap().unwrap();

        assert_eq!(request.id, id);
        assert_eq!(request.mode, mode);
        assert_eq!(request.questions.len(), 1);
        assert!(dialogs.contains(id));
    }

    dialogs.retire("credits").unwrap();

    assert!(dialogs.has_pending());
    assert!(!dialogs.contains("credits"));
    assert!(dialogs.contains("refusal"));
    assert!(dialogs.contains("form"));
    assert!(dialogs.contains("url"));
    assert_eq!(
        dialogs.respond("url", None).unwrap().response,
        json!({"action": "cancel"})
    );
}

#[test]
fn responses_remain_retryable_until_the_request_is_retired() {
    let mut dialogs = Dialogs::default();

    let request = dialogs
        .receive("refusal", &refusal_request())
        .unwrap()
        .unwrap();

    let answer = select(&request, 1);

    assert!(dialogs.respond("refusal", Some(&[])).is_err());
    assert!(dialogs.contains("refusal"));

    for _ in 0..2 {
        let reply = dialogs
            .respond("refusal", Some(slice::from_ref(&answer)))
            .unwrap();

        assert_eq!(
            reply.response,
            json!({"behavior": "completed", "result": "edit_prompt"})
        );
        assert!(reply.edit_prompt);
        assert_eq!(
            reply.retracted_messages,
            ["partial-response", "partial-reasoning"]
        );
        assert!(dialogs.contains("refusal"));
    }

    dialogs.retire("refusal").unwrap();

    assert!(dialogs.respond("refusal", Some(&[answer])).is_err());
    assert!(!dialogs.has_pending());
}

#[test]
fn cancellation_and_retirement_return_retractions_once_per_removal() {
    let mut dialogs = Dialogs::default();

    dialogs
        .receive("refusal", &refusal_request())
        .unwrap()
        .unwrap();

    let reply = dialogs.respond("refusal", None).unwrap();

    assert_eq!(reply.response, json!({"behavior": "cancelled"}));
    assert!(!reply.edit_prompt);
    assert_eq!(
        reply.retracted_messages,
        ["partial-response", "partial-reasoning"]
    );
    assert!(dialogs.contains("refusal"));
    assert_eq!(dialogs.retire("refusal").unwrap(), reply.retracted_messages);
    assert!(dialogs.retire("refusal").is_none());
    assert!(dialogs.retire("unknown").is_none());

    dialogs
        .receive("credits", &usage_request())
        .unwrap()
        .unwrap();

    assert!(
        dialogs
            .respond("credits", None)
            .unwrap()
            .retracted_messages
            .is_empty()
    );
    assert!(dialogs.retire("credits").unwrap().is_empty());
}

#[test]
fn form_submission_decline_and_cancel_preserve_the_pending_request() {
    let mut dialogs = Dialogs::default();

    dialogs.receive("form", &form_request()).unwrap().unwrap();

    let answer = QuestionAnswer {
        selected: Vec::new(),
        custom: Some("October release".into()),
    };

    let submitted = dialogs.respond("form", Some(&[answer])).unwrap();

    assert_eq!(
        submitted.response,
        json!({"action": "accept", "content": {"name": "October release"}})
    );
    assert!(!submitted.edit_prompt);
    assert!(submitted.retracted_messages.is_empty());
    assert_eq!(
        dialogs.decline("form").unwrap().response,
        json!({"action": "decline"})
    );
    assert_eq!(
        dialogs.respond("form", None).unwrap().response,
        json!({"action": "cancel"})
    );
    assert!(dialogs.contains("form"));

    dialogs
        .receive("refusal", &refusal_request())
        .unwrap()
        .unwrap();

    assert!(dialogs.decline("refusal").is_err());
    assert!(dialogs.decline("unknown").is_err());
    assert!(dialogs.contains("refusal"));
}

#[test]
fn finishing_a_turn_expires_inputs_without_removing_transcript_messages() {
    let mut control = ControlState::default();

    control
        .dialogs
        .receive("refusal", &refusal_request())
        .unwrap()
        .unwrap();

    control
        .dialogs
        .receive("form", &form_request())
        .unwrap()
        .unwrap();

    assert!(control.has_active_request());

    let events = control.finish_turn();

    let mut expired_ids = Vec::new();

    for event in events {
        match event {
            Event::InputResolved {
                id,
                resolution: QuestionResolution::Expired,
            } => expired_ids.push(id),
            _ => panic!("finishing a pending dialog must only expire its input"),
        }
    }

    expired_ids.sort();

    assert_eq!(expired_ids, ["form", "refusal"]);
    assert!(!control.has_active_request());
    assert!(control.finish_turn().is_empty());
    assert!(control.dialogs.retire("refusal").is_none());
    assert!(
        control
            .dialogs
            .receive("refusal", &refusal_request())
            .unwrap()
            .is_none()
    );
    assert!(
        control
            .dialogs
            .receive("form", &form_request())
            .unwrap()
            .is_none()
    );
}

#[test]
fn unsupported_or_invalid_requests_do_not_consume_the_request_id() {
    let mut dialogs = Dialogs::default();

    for request in [
        json!({"subtype": "request_user_dialog", "dialog_kind": "enum_prompt", "payload": {}}),
        json!({"subtype": "request_user_dialog", "dialog_kind": "future_dialog", "payload": {}}),
        json!({"subtype": "request_user_dialog", "payload": {}}),
        json!({"subtype": "request_user_dialog", "dialog_kind": "refusal_fallback_prompt", "payload": {}}),
        json!({"subtype": "elicitation", "mcp_server_name": "tools", "message": "Missing schema"}),
    ] {
        assert!(dialogs.receive("request", &request).is_err());
        assert!(!dialogs.contains("request"));
        assert!(!dialogs.has_pending());
    }

    assert!(dialogs.receive("", &refusal_request()).is_err());
    assert!(!dialogs.has_pending());
    assert!(
        dialogs
            .receive("request", &refusal_request())
            .unwrap()
            .is_some()
    );
    assert!(dialogs.contains("request"));
}
