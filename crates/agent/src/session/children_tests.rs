use crate::background_task::{
    BackgroundTaskTranscriptState as LoadState, BackgroundTaskTranscriptUpdate as Update,
    MAX_TRANSCRIPT_ITEMS,
};
use crate::chat::Item;
use crate::session::children::ChildTranscript;

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
