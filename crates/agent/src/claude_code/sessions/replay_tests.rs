use serde_json::{Value, json};

use crate::chat::{Item, ReplayTurn};
use crate::claude_code::sessions::replay::{parse_child_replay, parse_replay};

fn transcript(records: &[Value]) -> String {
    records
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

fn items(turns: &[ReplayTurn]) -> Vec<&Item> {
    turns
        .iter()
        .flat_map(|turn| turn.items.iter().map(|entry| &entry.item))
        .collect()
}

#[test]
fn fallback_replay_removes_retracted_and_superseded_output_without_changing_turns() {
    let text = transcript(&[
        json!({"type": "user", "uuid": "prompt", "message": {"content": "first prompt"}}),
        json!({"type": "assistant", "uuid": "old", "parentUuid": "prompt", "message": {
            "usage": {"output_tokens": 10}, "content": [
                {"type": "text", "text": "discarded reply"},
                {"type": "thinking", "thinking": "discarded reasoning"},
                {"type": "tool_use", "id": "command", "name": "Bash", "input": {"command": "pwd"}}
            ]
        }}),
        json!({"type": "user", "uuid": "result", "parentUuid": "old", "message": {
            "content": [{"type": "tool_result", "tool_use_id": "command", "content": "discarded result"}]
        }}),
        json!({"type": "assistant", "uuid": "partial", "parentUuid": "result", "message": {
            "usage": {"output_tokens": 20}, "content": [{"type": "text", "text": "intermediate reply"}]
        }}),
        json!({"type": "assistant", "uuid": "replacement", "parentUuid": "partial", "supersedesUuids": ["partial"], "message": {
            "usage": {"output_tokens": 30}, "content": [{"type": "text", "text": "replacement reply"}]
        }}),
        json!({"type": "system", "uuid": "notice", "parentUuid": "replacement", "subtype": "model_refusal_fallback",
            "content": "Switched models after refusal.", "retractedMessageUuids": ["old", "result", "partial"]}),
        json!({"type": "system", "uuid": "duration", "parentUuid": "replacement", "subtype": "turn_duration", "durationMs": 12000}),
        json!({"type": "user", "uuid": "next-prompt", "parentUuid": "replacement", "message": {"content": "next prompt"}}),
        json!({"type": "assistant", "uuid": "next-reply", "parentUuid": "next-prompt", "message": {
            "usage": {"output_tokens": 5}, "content": [{"type": "text", "text": "next reply"}]
        }}),
    ]);

    let turns = parse_replay(text.as_bytes());
    let shown = items(&turns);

    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0].seconds, Some(12));
    assert_eq!(turns[0].output_tokens, Some(60));
    assert_eq!(turns[1].output_tokens, Some(5));
    assert_eq!(shown.len(), 5);
    assert!(
        matches!(shown[1], Item::AgentMessage { text: Some(text), .. } if text == "replacement reply")
    );
    assert!(
        matches!(shown[2], Item::Other { id, output: Some(output), .. }
        if id == "model-notice-notice" && output == "Switched models after refusal.")
    );
    assert!(
        matches!(shown[4], Item::AgentMessage { text: Some(text), .. } if text == "next reply")
    );
}

#[test]
fn final_model_notice_removes_a_tool_by_its_result_uuid() {
    let text = transcript(&[
        json!({"type": "user", "uuid": "prompt", "message": {"content": "prompt"}}),
        json!({"type": "assistant", "uuid": "call", "parentUuid": "prompt", "message": {"content": [
            {"type": "text", "text": "retained text"},
            {"type": "tool_use", "id": "tool", "name": "Grep", "input": {"pattern": "query"}}
        ]}}),
        json!({"type": "user", "uuid": "result", "parentUuid": "call", "message": {"content": [
            {"type": "tool_result", "tool_use_id": "tool", "content": "removed result"}
        ]}}),
        json!({"type": "system", "uuid": "notice", "parentUuid": "result", "subtype": "model_fallback",
            "content": "Fallback active.", "retracted_message_uuids": ["result"]}),
    ]);

    let turns = parse_replay(text.as_bytes());
    let shown = items(&turns);

    assert_eq!(shown.len(), 3);
    assert!(
        matches!(shown[1], Item::AgentMessage { text: Some(text), .. } if text == "retained text")
    );
    assert!(matches!(shown[2], Item::Other { kind, .. } if kind == "model_fallback"));
    assert!(!shown.iter().any(|item| item.id() == Some("tool")));
}

#[test]
fn notices_from_an_abandoned_branch_do_not_remove_active_output() {
    let text = transcript(&[
        json!({"type": "user", "uuid": "prompt", "message": {"content": "prompt"}}),
        json!({"type": "assistant", "uuid": "abandoned", "parentUuid": "prompt", "message": {"content": [{"type": "text", "text": "old branch"}]}}),
        json!({"type": "system", "uuid": "notice", "parentUuid": "abandoned", "subtype": "model_refusal_fallback",
            "content": "Old branch notice.", "retractedMessageUuids": ["active"]}),
        json!({"type": "assistant", "uuid": "active", "parentUuid": "prompt", "message": {"content": [{"type": "text", "text": "active branch"}]}}),
    ]);

    let turns = parse_replay(text.as_bytes());
    let shown = items(&turns);

    assert_eq!(shown.len(), 2);
    assert!(
        matches!(shown[1], Item::AgentMessage { text: Some(text), .. } if text == "active branch")
    );
}

#[test]
fn child_replay_applies_saved_supersession_and_model_notices() {
    let text = transcript(&[
        json!({"type": "user", "uuid": "prompt", "isSidechain": true, "message": {"content": "child prompt"}}),
        json!({"type": "assistant", "uuid": "old", "parentUuid": "prompt", "isSidechain": true,
            "message": {"content": [{"type": "text", "text": "old child reply"}]}}),
        json!({"type": "assistant", "uuid": "new", "parentUuid": "old", "isSidechain": true, "supersedes_uuids": ["old"],
            "message": {"content": [{"type": "text", "text": "new child reply"}]}}),
        json!({"type": "system", "uuid": "notice", "parentUuid": "new", "isSidechain": true,
            "subtype": "model_consent_fallback", "originalModel": "first", "fallbackModel": "second"}),
    ]);

    let shown = parse_child_replay(text.as_bytes());

    assert_eq!(shown.len(), 3);
    assert!(
        matches!(&shown[1], Item::AgentMessage { text: Some(text), .. } if text == "new child reply")
    );
    assert!(matches!(&shown[2], Item::Other { kind, .. } if kind == "model_consent_fallback"));
}
