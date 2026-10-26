use serde_json::{Value, json};

use crate::chat::{Event, Item, SlashCommandOutcome, ThreadSettings};
use crate::claude_code::stream_json::transcript::TranscriptState;
use crate::session::controller::SessionController;
use crate::session::test_support::TestBackend;
use crate::session::{AgentKind, Backend};

fn assistant(uuid: &str, content: Value) -> Value {
    json!({"type": "assistant", "uuid": uuid, "message": {"content": content}})
}

fn removed_ids(events: &[Event]) -> Vec<&str> {
    events
        .iter()
        .flat_map(|event| match event {
            Event::ItemsRemoved { item_ids } => item_ids.iter().map(String::as_str).collect(),
            _ => Vec::new(),
        })
        .collect()
}

#[test]
fn retracting_message_removes_all_streamed_blocks_and_pending_tools_once() {
    let mut state = TranscriptState::default();
    let mut started_ids = Vec::new();

    for (index, kind) in ["text", "thinking", "text"].into_iter().enumerate() {
        let events = state.on_stream_event(&json!({
            "event": {"type": "content_block_start", "index": index,
                "content_block": {"type": kind}}
        }));

        let Event::ItemStarted(item) = &events[0] else {
            panic!("content block did not create an item");
        };

        started_ids.push(item.id().unwrap().to_owned());
    }

    let snapshot = assistant(
        "response",
        json!([
            {"type": "text", "text": "first"},
            {"type": "thinking", "thinking": "reason"},
            {"type": "text", "text": "second"},
            {"type": "tool_use", "id": "command", "name": "Bash", "input": {"command": "pwd"}}
        ]),
    );

    state.on_assistant(&snapshot);
    started_ids.push("command".to_owned());

    let uuids = [
        "response".to_owned(),
        "unknown".to_owned(),
        "response".to_owned(),
    ];

    let events = state.retract_messages(&uuids);

    assert_eq!(removed_ids(&events), started_ids);
    assert!(state.retract_messages(&uuids).is_empty());
    assert!(state.on_assistant(&snapshot).is_empty());
    assert!(
        state
            .on_stream_event(&json!({
                "event": {"type": "content_block_delta", "index": 0,
                    "delta": {"type": "text_delta", "text": "stale"}}
            }))
            .is_empty()
    );
    assert!(
        state
            .on_user_message(&json!({
                "type": "user", "uuid": "result", "message": {"content": [
                    {"type": "tool_result", "tool_use_id": "command", "content": "stale"}
                ]}
            }))
            .is_empty()
    );
}

#[test]
fn tool_result_uuid_removes_the_completed_call_row() {
    let mut state = TranscriptState::default();

    state.on_assistant(&assistant(
        "call",
        json!([
            {"type": "tool_use", "id": "search", "name": "Grep", "input": {"pattern": "text"}}
        ]),
    ));

    state.on_user_message(&json!({
        "type": "user", "uuid": "result", "message": {"content": [
            {"type": "tool_result", "tool_use_id": "search", "content": "found"}
        ]}
    }));

    let events = state.retract_messages(&["result".to_owned()]);

    assert_eq!(removed_ids(&events), ["search"]);
    assert!(state.retract_messages(&["call".to_owned()]).is_empty());
}

#[test]
fn superseding_snapshot_removes_prior_output_before_the_replacement() {
    for field in ["supersedes_uuids", "supersedesUuids"] {
        let mut state = TranscriptState::default();

        state.on_assistant(&assistant("old", json!([{"type": "text", "text": "old"}])));

        let mut replacement = assistant("new", json!([{"type": "text", "text": "new"}]));

        replacement[field] = json!(["old"]);

        let events = state.on_assistant(&replacement);

        assert!(matches!(&events[0], Event::ItemsRemoved { item_ids } if item_ids == &["text-1"]));
        assert!(
            matches!(&events[1], Event::ItemCompleted(Item::AgentMessage {
            id, text: Some(text), ..
        }) if id == "text-2" && text == "new")
        );
        assert!(state.retract_messages(&["old".to_owned()]).is_empty());
    }
}

#[test]
fn repeated_snapshots_map_every_item_without_removing_another_message() {
    let mut state = TranscriptState::default();

    for (uuid, text) in [("first", "one"), ("second", "two"), ("first", "three")] {
        state.on_assistant(&assistant(uuid, json!([{"type": "text", "text": text}])));
    }

    let removed = state.retract_messages(&["first".to_owned()]);

    assert_eq!(removed_ids(&removed), ["text-1", "text-3"]);
    assert_eq!(
        removed_ids(&state.retract_messages(&["second".to_owned()])),
        ["text-2"]
    );
}

#[test]
fn applied_retraction_clears_current_turn_and_late_records_do_not_restore_it() {
    let mut session = SessionController::new(AgentKind::Claude);

    let epoch = session.starting(None);
    let backend = TestBackend::new(Vec::new(), SlashCommandOutcome::NotReady, Vec::new());

    session.install(epoch, Ok(Backend::Test(backend)));
    session.apply_event(epoch, Event::Ready(ThreadSettings::default()));
    session.apply_event(epoch, Event::TurnStarted);

    let turn = session.delivery().turn();

    let mut state = TranscriptState::default();

    let snapshot = assistant(
        "old",
        json!([
            {"type": "text", "text": "removed reply"},
            {"type": "thinking", "thinking": "removed reasoning"},
            {"type": "tool_use", "id": "tool", "name": "Bash", "input": {"command": "pwd"}}
        ]),
    );

    let result = json!({"type": "user", "uuid": "result", "message": {"content": [
        {"type": "tool_result", "tool_use_id": "tool", "content": "removed output"}
    ]}});

    for event in state
        .on_assistant(&snapshot)
        .into_iter()
        .chain(state.on_user_message(&result))
    {
        session.apply_event(epoch, event);
    }

    assert_eq!(session.conversation().borrow().content.entries().len(), 3);

    for event in state.retract_messages(&["old".into(), "result".into()]) {
        session.apply_event(epoch, event);
    }

    assert!(session.conversation().borrow().content.entries().is_empty());
    assert!(state.on_assistant(&snapshot).is_empty());
    assert!(state.on_user_message(&result).is_empty());

    for event in state.on_assistant(&assistant(
        "new",
        json!([{"type": "text", "text": "replacement"}]),
    )) {
        session.apply_event(epoch, event);
    }

    let conversation = session.conversation().borrow();

    assert_eq!(conversation.content.entries().len(), 1);
    assert_eq!(conversation.content.entries()[0].turn, turn);
    assert_eq!(
        conversation.content.latest_agent_message(turn),
        Some("replacement")
    );
    assert_eq!(conversation.content.turn_steps(turn), 0);
}
