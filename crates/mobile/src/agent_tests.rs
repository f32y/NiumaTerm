use nmt_agent::chat::{Item, Question, QuestionInput, QuestionOption};
use nmt_agent::session::AgentKind;
use nmt_agent::session::controller::SessionController;
use nmt_agent::session::view::{ViewEntry, ViewOp, ViewPublisher};
use nmt_agent::transcript::TranscriptEntry;
use nmt_agent::transcript::conversation::EntryMetadata;
use nmt_remote::connection::AgentUpdate;
use serde_json::{Value, json};

use crate::agent::{Replica, draft_answers};
use crate::records::QuestionAnswer;

fn question(multi_select: bool, input: QuestionInput) -> Question {
    Question {
        header: None,
        question: "Which?".into(),
        multi_select,
        options: ["a", "b", "c"]
            .into_iter()
            .map(|label| QuestionOption {
                label: label.into(),
                description: None,
            })
            .collect(),
        input,
    }
}

fn answer(selected: &[u32], text: Option<&str>) -> QuestionAnswer {
    QuestionAnswer {
        selected: selected.to_vec(),
        text: text.map(str::to_owned),
    }
}

#[test]
fn answers_take_the_shape_each_question_allows() {
    let questions = [
        question(false, QuestionInput::SelectionOnly),
        question(true, QuestionInput::SelectionOnly),
        question(false, QuestionInput::Text),
    ];

    let draft = draft_answers(
        &questions,
        vec![
            // One choice keeps one pick, and a question without text
            // ignores typed text.
            answer(&[2, 0], Some("typed")),
            // Out-of-range and repeated picks are dropped.
            answer(&[2, 9, 0, 2], None),
            answer(&[1], Some("my own")),
        ],
    )
    .unwrap();

    assert_eq!(draft.selected, [vec![2], vec![0, 2], vec![1]]);
    assert_eq!(draft.custom, [false, false, true]);
    assert_eq!(draft.text, ["", "", "my own"]);
}

#[test]
fn answers_for_a_different_number_of_questions_are_refused() {
    let questions = [question(false, QuestionInput::SelectionOnly)];

    assert!(draft_answers(&questions, Vec::new()).is_none());
}

fn transcript(from: usize, end: usize) -> Vec<TranscriptEntry<EntryMetadata>> {
    (from..end)
        .map(|index| TranscriptEntry {
            turn: index as u64,
            item: Item::AgentMessage {
                id: index.to_string(),
                text: Some(format!("entry {index}")),
                questions: None,
            },
            metadata: EntryMetadata::default(),
        })
        .collect()
}

fn controller(count: usize) -> SessionController {
    let mut controller = SessionController::new(AgentKind::Codex);

    controller.splice_transcript(0, transcript(0, count));

    controller
}

fn event(kind: &str, value: Value) -> AgentUpdate {
    AgentUpdate::Paged(json!({"view_id":"first", "type":kind, "value":value}))
}

#[test]
fn earlier_pages_prepend_without_losing_the_live_tail() {
    let mut controller = controller(53);
    let mut publisher = ViewPublisher::default();

    let snapshot = publisher.snapshot_paged(&controller);

    let mut replica = Replica::default();

    assert_eq!(replica.apply(event("snapshot", json!(snapshot))), Some(0));

    let request = replica.begin_earlier().unwrap();

    assert!(replica.begin_earlier().is_none());

    controller.splice_transcript(53, transcript(53, 55));

    let update = publisher.changes_paged(&controller).unwrap();

    assert_eq!(replica.apply(event("update", json!(update))), Some(20));
    assert_eq!(replica.load_epoch, request.0);

    let page = publisher
        .earlier_paged(&controller, snapshot.cursor)
        .unwrap();

    assert_eq!(replica.apply(event("page", json!(page))), Some(0));
    assert_eq!(replica.cursor.unwrap().start, 13);
    assert_eq!(replica.cursor.unwrap().loaded_blocks, 2);
    assert!(!replica.loading_earlier);

    let turns: Vec<_> = replica
        .view
        .as_ref()
        .unwrap()
        .transcript
        .iter()
        .map(|entry| entry.turn)
        .collect();

    assert_eq!(turns, (13..55).collect::<Vec<_>>());
}

#[test]
fn reconnect_replaces_the_window_and_rejects_old_view_pages() {
    let controller = controller(80);

    let mut publisher = ViewPublisher::default();

    let snapshot = publisher.snapshot_paged(&controller);

    let mut replica = Replica::default();

    replica.apply(event("snapshot", json!(snapshot)));

    let first_request = replica.begin_earlier().unwrap();

    let page = publisher
        .earlier_paged(&controller, snapshot.cursor)
        .unwrap();

    let fresh = ViewPublisher::default().snapshot_paged(&controller);

    replica.apply(AgentUpdate::Paged(json!({
        "view_id":"second", "type":"snapshot", "value":fresh,
    })));

    assert_ne!(replica.load_epoch, first_request.0);
    assert!(!replica.loading_earlier);
    assert_eq!(replica.apply(event("page", json!(page))), None);
    assert_eq!(replica.cursor, Some(fresh.cursor));
    assert_eq!(replica.view.unwrap().transcript.len(), 20);
}

#[test]
fn stale_or_malformed_updates_leave_the_loaded_view_unchanged() {
    let mut controller = controller(53);
    let mut publisher = ViewPublisher::default();

    let snapshot = publisher.snapshot_paged(&controller);

    let mut replica = Replica::default();

    replica.apply(event("snapshot", json!(snapshot)));
    controller.splice_transcript(53, transcript(53, 54));

    let mut update = publisher.changes_paged(&controller).unwrap();

    update.ops.push(ViewOp::Splice {
        from: usize::MAX,
        entries: Vec::new(),
    });

    assert_eq!(replica.apply(event("update", json!(update))), None);
    assert_eq!(replica.cursor, Some(snapshot.cursor));
    assert_eq!(
        replica.view.as_ref().unwrap().transcript,
        snapshot.view.transcript
    );

    let page = publisher
        .earlier_paged(&controller, snapshot.cursor)
        .unwrap();

    assert_eq!(replica.apply(event("page", json!(page))), None);
    assert_eq!(replica.view.unwrap().transcript.len(), 20);
}

#[test]
fn page_event_applies_its_catch_up_before_prepending() {
    let mut controller = controller(53);
    let mut publisher = ViewPublisher::default();

    let snapshot = publisher.snapshot_paged(&controller);

    let mut replica = Replica::default();

    replica.apply(event("snapshot", json!(snapshot)));
    replica.begin_earlier();
    controller.splice_transcript(53, transcript(53, 54));

    let update = publisher.changes_paged(&controller).unwrap();

    let page = publisher
        .earlier_paged(&controller, snapshot.cursor)
        .unwrap();

    assert_eq!(
        replica.apply(AgentUpdate::Paged(json!({
            "view_id":"first", "type":"page", "catch_up":update, "value":page,
        }))),
        Some(0)
    );
    assert_eq!(replica.cursor, Some(page.cursor));
    assert_eq!(replica.view.unwrap().transcript.len(), 41);
}

#[test]
fn unsupported_host_error_finishes_loading_and_is_exposed() {
    let controller = controller(53);
    let snapshot = ViewPublisher::default().snapshot_paged(&controller);

    let mut replica = Replica::default();

    replica.apply(event("snapshot", json!(snapshot)));

    let request = replica.begin_earlier().unwrap();

    replica.apply(AgentUpdate::Error(
        "Update the computer to support paging.".into(),
    ));

    assert!(!replica.loading_earlier);
    assert_ne!(replica.load_epoch, request.0);
    assert_eq!(
        replica.history_error.as_deref(),
        Some("Update the computer to support paging.")
    );
}

#[test]
fn a_late_request_failure_cannot_replace_a_new_loading_state() {
    let controller = controller(53);
    let snapshot = ViewPublisher::default().snapshot_paged(&controller);

    let mut replica = Replica::default();

    replica.apply(event("snapshot", json!(snapshot)));

    let old = replica.begin_earlier().unwrap();

    replica.apply(event("snapshot", json!(snapshot)));

    let current = replica.begin_earlier().unwrap();

    assert_ne!(old.0, current.0);
    assert!(
        replica
            .finish_earlier(old.0, Err(anyhow::anyhow!("old request failed")))
            .is_ok()
    );
    assert!(replica.loading_earlier);
    assert!(replica.history_error.is_none());
    assert!(
        replica
            .finish_earlier(current.0, Err(anyhow::anyhow!("current request failed")))
            .is_err()
    );
    assert!(!replica.loading_earlier);
    assert_eq!(
        replica.history_error.as_deref(),
        Some("current request failed")
    );
}

#[test]
fn one_large_tool_entry_is_not_truncated_by_paging_replication() {
    let controller = controller(0);

    let mut snapshot = ViewPublisher::default().snapshot_paged(&controller);

    let text = "x".repeat(9 * 1024 * 1024);

    snapshot.view.transcript.push(ViewEntry {
        turn: 1,
        item: Item::Other {
            id: "large-tool".into(),
            kind: "tool".into(),
            title: "Tool result".into(),
            output: Some(text.clone()),
            status: Some("completed".into()),
        },
        at: None,
        images: Vec::new(),
    });

    snapshot.cursor.end = 1;
    snapshot.cursor.loaded_blocks = 1;

    let mut replica = Replica::default();

    assert_eq!(replica.apply(event("snapshot", json!(snapshot))), Some(0));

    let Item::Other { output, .. } = &replica.view.as_ref().unwrap().transcript[0].item else {
        panic!("tool result entry");
    };

    assert_eq!(output.as_ref(), Some(&text));
}

#[test]
fn oversized_update_error_retains_transcript_and_finishes_loading() {
    let controller = controller(53);
    let snapshot = ViewPublisher::default().snapshot_paged(&controller);

    let mut replica = Replica::default();

    replica.apply(event("snapshot", json!(snapshot)));

    let request = replica.begin_earlier().unwrap();

    assert_eq!(
        replica.apply(event(
            "error",
            json!({"message":"This update exceeds the transfer limit."})
        )),
        None
    );
    assert_eq!(replica.cursor, Some(snapshot.cursor));
    assert_eq!(
        replica.view.as_ref().unwrap().transcript,
        snapshot.view.transcript
    );
    assert_eq!(
        replica.history_error.as_deref(),
        Some("This update exceeds the transfer limit.")
    );
    assert!(!replica.loading_earlier);
    assert_ne!(replica.load_epoch, request.0);
    assert!(
        replica
            .finish_earlier(request.0, Err(anyhow::anyhow!("late RPC failure")))
            .is_ok()
    );
    assert_eq!(
        replica.history_error.as_deref(),
        Some("This update exceeds the transfer limit.")
    );
}

#[test]
fn an_error_from_an_old_view_cannot_interrupt_the_current_request() {
    let controller = controller(53);
    let snapshot = ViewPublisher::default().snapshot_paged(&controller);

    let mut replica = Replica::default();

    replica.apply(event("snapshot", json!(snapshot)));

    let request = replica.begin_earlier().unwrap();

    assert_eq!(
        replica.apply(AgentUpdate::Paged(json!({
            "view_id":"old", "type":"error", "value":{"message":"Old transfer failed."},
        }))),
        None
    );
    assert!(replica.loading_earlier);
    assert_eq!(replica.load_epoch, request.0);
    assert!(replica.history_error.is_none());
    assert_eq!(replica.cursor, Some(snapshot.cursor));
}

#[test]
fn an_error_before_the_first_snapshot_is_visible_and_a_new_snapshot_recovers() {
    let mut replica = Replica::default();

    replica.apply(event(
        "error",
        json!({"message":"Initial page exceeds the transfer limit."}),
    ));

    assert!(replica.view.is_none());
    assert_eq!(
        replica.history_error.as_deref(),
        Some("Initial page exceeds the transfer limit.")
    );

    let controller = controller(53);
    let snapshot = ViewPublisher::default().snapshot_paged(&controller);

    replica.apply(event("snapshot", json!(snapshot)));

    assert!(replica.history_error.is_none());
    assert_eq!(replica.cursor, Some(snapshot.cursor));
    assert_eq!(replica.view.as_ref().unwrap().transcript.len(), 20);
}
