use std::cell::RefCell;

use serde_json::{Value, json};

use crate::deepseek::api::ApiClient;
use crate::deepseek::events::streams::Streams;
use crate::deepseek::mapping::{approval_request, question_request};

fn item(stream: &str, value: Value) -> Value {
    json!({ "type": "item", "streamId": stream, "value": value })
}

#[test]
fn readiness_waits_for_all_subscriptions_and_delivers_the_opening_history() {
    let client = ApiClient::new("http://127.0.0.1:1".into()).unwrap();
    let frames = RefCell::new(Vec::new());
    let deliver = |frame| frames.borrow_mut().push(frame);
    let mut streams = Streams::new("session-1");
    streams
        .process(
            item(
                "events",
                json!({ "type": "ready", "clientId": "generation-1" }),
            ),
            &client,
            &deliver,
        )
        .unwrap();
    streams
        .process(
            item(
                "control",
                json!({ "type": "baseline", "value": { "queues": {}, "projections": {} } }),
            ),
            &client,
            &deliver,
        )
        .unwrap();
    assert!(streams.ready_snapshot().is_none());
    let snapshot = json!({ "type": "snapshot", "cursor": 4, "records": [], "projections": { "asOfSeq": 4, "values": {} } });
    streams
        .process(item("follow", snapshot.clone()), &client, &deliver)
        .unwrap();
    assert_eq!(streams.ready_snapshot(), Some(&snapshot));
    assert_eq!(frames.borrow().last().unwrap()["payload"]["page"], snapshot);
}

#[test]
fn control_updates_do_not_cross_sessions_and_keep_their_cursor() {
    let client = ApiClient::new("http://127.0.0.1:1".into()).unwrap();
    let frames = RefCell::new(Vec::new());
    let deliver = |frame| frames.borrow_mut().push(frame);
    let mut streams = Streams::new("session-1");
    streams
        .process(
            item(
                "control",
                json!({ "type": "queue", "sessionId": "session-2", "items": [] }),
            ),
            &client,
            &deliver,
        )
        .unwrap();
    assert!(frames.borrow().is_empty());
    streams.process(item("control", json!({ "type": "projection", "sessionId": "session-1", "key": "title", "value": "New title", "seq": 9 })), &client, &deliver).unwrap();
    assert_eq!(
        frames.borrow()[0]["payload"],
        json!({ "type": "session/projection", "sessionId": "session-1", "key": "title", "value": "New title", "seq": 9 })
    );
}

#[test]
fn interactions_retain_the_generation_and_cancel_the_matching_card() {
    let client = ApiClient::new("http://127.0.0.1:1".into()).unwrap();
    let frames = RefCell::new(Vec::new());
    let deliver = |frame| frames.borrow_mut().push(frame);
    let mut streams = Streams::new("session-1");
    streams
        .process(
            item(
                "events",
                json!({ "type": "ready", "clientId": "generation-1" }),
            ),
            &client,
            &deliver,
        )
        .unwrap();
    streams.process(item("events", json!({
        "type": "waterfall", "event": "approval/request", "eventId": "approval-1", "agentId": "session-1",
        "request": { "toolName": "pwsh", "reason": "Write outside the workspace" },
    })), &client, &deliver).unwrap();
    let approval = approval_request(&frames.borrow()[0], "session-1").unwrap();
    assert_eq!(approval.client_id, "generation-1");
    assert_eq!(approval.event_id, "approval-1");
    streams.process(item("events", json!({
        "type": "waterfall", "event": "user-questions/request", "eventId": "question-1", "agentId": "session-1",
        "request": { "questions": [{ "id": "q1", "question": "Continue?", "options": [{ "label": "Yes" }] }] },
    })), &client, &deliver).unwrap();
    let (questions, _) = question_request(&frames.borrow()[1], "session-1").unwrap();
    assert_eq!(questions.event_id, "question-1");
    streams
        .process(
            item(
                "events",
                json!({ "type": "cancel", "eventId": "approval-1" }),
            ),
            &client,
            &deliver,
        )
        .unwrap();
    assert_eq!(
        frames.borrow().last().unwrap()["payload"]["type"],
        "approval/resolved"
    );
    streams
        .process(
            item(
                "events",
                json!({ "type": "cancel", "eventId": "question-1" }),
            ),
            &client,
            &deliver,
        )
        .unwrap();
    assert_eq!(
        frames.borrow().last().unwrap()["payload"]["type"],
        "question/resolved"
    );
}

#[test]
fn a_failed_subscription_is_a_startup_failure() {
    let client = ApiClient::new("http://127.0.0.1:1".into()).unwrap();
    let error = Streams::new("session-1")
        .process(
            json!({
                "type": "error", "streamId": "follow", "error": { "message": "session not found" },
            }),
            &client,
            &|_| {},
        )
        .unwrap_err();
    assert!(error.contains("session not found"));
}
