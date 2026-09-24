use crate::background_task::{
    BackgroundTaskKey, BackgroundTaskLoadState as LoadState, BackgroundTaskRegistry,
    BackgroundTaskSnapshot, BackgroundTaskState, BackgroundTaskTranscriptUpdate as Update,
    BackgroundTaskUpdate, MAX_TRANSCRIPT_ITEMS,
};
use crate::chat::Item;
use crate::session::children::{ChildAgents, ChildTranscript};

fn message(id: &str, text: &str) -> Item {
    Item::AgentMessage {
        id: id.into(),
        text: Some(text.into()),
        questions: None,
    }
}

#[test]
fn completion_merges_into_streamed_content() {
    let mut transcript = ChildTranscript::default();

    transcript.apply(Update::appended(vec![Item::CommandExecution {
        id: "command".into(),
        command: "echo result".into(),
        purpose: Some("Check output".into()),
        aggregated_output: Some("streamed".into()),
        status: Some("inProgress".into()),
        exit_code: None,
    }]));

    transcript.apply(Update::appended(vec![Item::CommandExecution {
        id: "command".into(),
        command: "echo result".into(),
        purpose: None,
        aggregated_output: None,
        status: Some("completed".into()),
        exit_code: Some(0),
    }]));

    let conversation = transcript.conversation.borrow();
    let entries = conversation.content.entries();

    assert_eq!(entries.len(), 1);

    let Item::CommandExecution {
        purpose,
        aggregated_output,
        status,
        exit_code,
        ..
    } = &entries[0].item
    else {
        panic!("expected command output");
    };

    assert_eq!(purpose.as_deref(), Some("Check output"));
    assert_eq!(aggregated_output.as_deref(), Some("streamed"));
    assert_eq!(status.as_deref(), Some("completed"));
    assert_eq!(*exit_code, Some(0));
}

#[test]
fn retention_keeps_newest_items_and_replacement_resets_missing_count() {
    let mut transcript = ChildTranscript::default();

    transcript.apply(Update::appended(
        (0..MAX_TRANSCRIPT_ITEMS + 10)
            .map(|index| message(&format!("m{index}"), "line"))
            .collect(),
    ));

    {
        let conversation = transcript.conversation.borrow();
        let entries = conversation.content.entries();

        assert_eq!(entries.len(), MAX_TRANSCRIPT_ITEMS);
        assert_eq!(entries[0].item.id(), Some("m10"));
    }

    assert_eq!(transcript.dropped(), 10);
    assert!(transcript.apply(Update::loaded(vec![message("only", "complete read")])));
    assert_eq!(transcript.dropped(), 0);

    let conversation = transcript.conversation.borrow();

    assert_eq!(conversation.content.entries().len(), 1);
    assert_eq!(conversation.content.entries()[0].item.id(), Some("only"));
}

#[test]
fn history_fills_empty_content_and_preserves_newer_live_content() {
    let mut transcript = ChildTranscript::default();

    assert!(transcript.apply(Update::restored(vec![message("history", "old")])));
    assert_eq!(
        transcript.conversation.borrow().content.entries()[0]
            .item
            .id(),
        Some("history")
    );

    transcript.apply(Update::loaded(vec![message("live", "new")]));

    assert!(!transcript.apply(Update::restored(vec![message("history", "old")])));
    assert_eq!(
        transcript.conversation.borrow().content.entries()[0]
            .item
            .id(),
        Some("live")
    );
}

#[test]
fn empty_and_identical_updates_do_not_request_refresh() {
    let mut transcript = ChildTranscript::default();

    assert!(transcript.apply(Update::state(LoadState::Loading)));
    assert!(!transcript.apply(Update::state(LoadState::Loading)));
    assert!(transcript.apply(Update::appended(vec![message("a", "one")])));
    assert!(!transcript.apply(Update::appended(Vec::new())));
    assert!(!transcript.apply(Update::loaded(vec![message("a", "one")])));
}

#[test]
fn failed_load_retains_known_content_and_reports_unavailable() {
    let mut transcript = ChildTranscript::default();

    transcript.apply(Update::appended(vec![message("a", "partial")]));

    assert!(transcript.apply(Update::state(LoadState::Unavailable {
        message: "read failed".into()
    })));
    assert_eq!(transcript.conversation.borrow().content.entries().len(), 1);
    assert!(
        matches!(transcript.state(), LoadState::Unavailable { message } if message == "read failed")
    );
}

fn snapshot_for(parent: BackgroundTaskKey) -> BackgroundTaskSnapshot {
    let mut registry = BackgroundTaskRegistry::new(parent);

    registry.apply(
        BackgroundTaskKey::codex("child-1"),
        BackgroundTaskUpdate::state(BackgroundTaskState::Working),
    );

    registry.snapshot()
}

#[test]
fn a_snapshot_is_shown_only_for_the_session_it_describes() {
    let codex = BackgroundTaskKey::codex("thread-a");

    let mut children = ChildAgents::default();

    children.set_snapshot(Some(&codex), snapshot_for(codex.clone()));

    assert!(children.scoped(Some(&codex)).is_some());
    assert!(
        children
            .scoped(Some(&BackgroundTaskKey::codex("thread-b")))
            .is_none()
    );
    assert!(
        children
            .scoped(Some(&BackgroundTaskKey::claude_code("thread-a")))
            .is_none(),
        "a Claude session must not adopt a Codex thread's rows"
    );
    assert!(
        children.scoped(None).is_none(),
        "an unsupported or not-yet-started pane shows no rows"
    );
}

#[test]
fn a_later_snapshot_replaces_the_previous_one_and_carries_its_activity() {
    let parent = BackgroundTaskKey::claude_code("session-1");

    let mut registry = BackgroundTaskRegistry::new(parent.clone());

    registry.apply(
        BackgroundTaskKey::claude_code("task-1"),
        BackgroundTaskUpdate::state(BackgroundTaskState::Working),
    );

    let first = registry.snapshot();

    registry.apply(
        BackgroundTaskKey::claude_code("task-1"),
        BackgroundTaskUpdate::state(BackgroundTaskState::Done),
    );

    let second = registry.snapshot();

    assert_eq!(first.active_count(), 1);
    assert_eq!(second.active_count(), 0);
    assert!(second.activity > first.activity);

    let mut children = ChildAgents::default();

    children.set_snapshot(Some(&parent), first);
    children.set_snapshot(Some(&parent), second.clone());

    assert_eq!(children.scoped(Some(&parent)), Some(&second));
}
