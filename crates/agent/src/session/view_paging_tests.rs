use crate::chat::Item;
use crate::session::AgentKind;
use crate::session::controller::SessionController;
use crate::session::view::{PagedAgentUpdate, PagedAgentView, ViewOp, ViewPublisher};
use crate::transcript::conversation::EntryMetadata;
use crate::transcript::{TextField, TranscriptEntry};

fn entries(from: usize, end: usize) -> Vec<TranscriptEntry<EntryMetadata>> {
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

fn session(count: usize) -> SessionController {
    let mut controller = SessionController::new(AgentKind::Codex);

    controller.splice_transcript(0, entries(0, count));

    controller
}

fn splice(update: &PagedAgentUpdate) -> (usize, usize) {
    update
        .ops
        .iter()
        .find_map(|op| match op {
            ViewOp::Splice { from, entries } => Some((*from, entries.len())),
            ViewOp::Slot { .. } => None,
        })
        .expect("transcript operation")
}

#[test]
fn first_page_and_earlier_pages_partition_the_transcript() {
    let controller = session(53);

    let mut publisher = ViewPublisher::default();

    let snapshot = publisher.snapshot_paged(&controller);

    assert_eq!(snapshot.view.transcript.len(), 20);
    assert_eq!((snapshot.cursor.start, snapshot.cursor.end), (33, 53));
    assert_eq!(snapshot.cursor.loaded_blocks, 1);

    let earlier = publisher
        .earlier_paged(&controller, snapshot.cursor)
        .unwrap();

    assert_eq!(earlier.before, snapshot.cursor);
    assert_eq!((earlier.cursor.start, earlier.entries.len()), (13, 20));
    assert_eq!(earlier.cursor.loaded_blocks, 2);

    let first = publisher
        .earlier_paged(&controller, earlier.cursor)
        .unwrap();

    assert_eq!((first.cursor.start, first.entries.len()), (0, 13));
    assert_eq!(first.cursor.loaded_blocks, 3);

    let repeated = publisher.earlier_paged(&controller, first.cursor).unwrap();

    assert!(repeated.entries.is_empty());
    assert_eq!(repeated.cursor, first.cursor);

    let all: Vec<_> = first
        .entries
        .into_iter()
        .chain(earlier.entries)
        .chain(snapshot.view.transcript)
        .map(|entry| entry.turn)
        .collect();

    assert_eq!(all, (0..53).collect::<Vec<_>>());
}

#[test]
fn append_uses_local_offsets_and_preserves_the_loaded_window() {
    let mut controller = session(53);
    let mut publisher = ViewPublisher::default();

    let snapshot = publisher.snapshot_paged(&controller);

    controller.splice_transcript(53, entries(53, 55));

    let update = publisher.changes_paged(&controller).unwrap();

    assert_eq!(update.before, snapshot.cursor);
    assert_eq!(splice(&update), (20, 2));
    assert_eq!((update.cursor.start, update.cursor.end), (33, 55));
    assert_eq!(update.cursor.loaded_blocks, 1);
    assert!(update.reset.is_none());
    assert!(publisher.changes_paged(&controller).is_none());
}

#[test]
fn stale_tail_revision_can_load_earlier_without_dropping_new_entries() {
    let mut controller = session(53);
    let mut publisher = ViewPublisher::default();

    let snapshot = publisher.snapshot_paged(&controller);

    controller.splice_transcript(53, entries(53, 54));

    let update = publisher.changes_paged(&controller).unwrap();

    let page = publisher
        .earlier_paged(&controller, snapshot.cursor)
        .unwrap();

    assert_eq!(page.before, update.cursor);
    assert_eq!((page.cursor.start, page.cursor.end), (13, 54));
    assert_eq!(page.entries.len(), 20);
    assert!(
        publisher
            .earlier_paged(&controller, snapshot.cursor)
            .is_err()
    );
}

#[test]
fn unknown_earlier_replacement_resets_to_one_bounded_page() {
    let mut controller = session(73);
    let mut publisher = ViewPublisher::default();

    let snapshot = publisher.snapshot_paged(&controller);

    let loaded = publisher
        .earlier_paged(&controller, snapshot.cursor)
        .unwrap();

    controller.splice_transcript(0, entries(100, 160));

    let update = publisher.changes_paged(&controller).unwrap();
    let reset = update.reset.unwrap();

    assert_eq!(reset.view.transcript.len(), 20);
    assert_eq!((reset.cursor.start, reset.cursor.end), (40, 60));
    assert_eq!(reset.cursor.loaded_blocks, 1);
    assert!(publisher.earlier_paged(&controller, loaded.cursor).is_err());
}

#[test]
fn generation_changes_reject_the_previous_conversations_cursor() {
    let mut controller = session(53);
    let mut publisher = ViewPublisher::default();

    let snapshot = publisher.snapshot_paged(&controller);

    controller.clear_conversation();
    controller.splice_transcript(0, entries(100, 135));

    let update = publisher.changes_paged(&controller).unwrap();

    assert!(update.reset.is_some());
    assert_ne!(snapshot.cursor.generation, update.cursor.generation);
    assert_eq!(update.cursor.end - update.cursor.start, 20);
    assert!(
        publisher
            .earlier_paged(&controller, snapshot.cursor)
            .is_err()
    );
}

#[test]
fn expired_change_history_does_not_fall_back_to_a_full_snapshot() {
    let controller = session(53);

    let mut publisher = ViewPublisher::default();

    publisher.snapshot_paged(&controller);

    for _ in 0..70 {
        controller
            .conversation()
            .borrow_mut()
            .append_delta("52", "x", TextField::Reply)
            .unwrap();
    }

    let reset = publisher.changes_paged(&controller).unwrap().reset.unwrap();

    assert_eq!(reset.view.transcript.len(), 20);
    assert_eq!(reset.cursor.loaded_blocks, 1);
}

#[test]
fn reconnect_starts_with_latest_twenty_after_several_pages_were_loaded() {
    let controller = session(100);

    let mut previous = ViewPublisher::default();

    let snapshot = previous.snapshot_paged(&controller);

    let page = previous
        .earlier_paged(&controller, snapshot.cursor)
        .unwrap();

    previous.earlier_paged(&controller, page.cursor).unwrap();

    let reconnected = ViewPublisher::default().snapshot_paged(&controller);

    assert_eq!(reconnected.cursor.start, 80);
    assert_eq!(reconnected.cursor.loaded_blocks, 1);
    assert_eq!(reconnected.view.transcript.len(), 20);
}

#[test]
fn an_empty_session_remains_pageable_after_its_first_append() {
    let mut controller = session(0);
    let mut publisher = ViewPublisher::default();

    let snapshot = publisher.snapshot_paged(&controller);

    assert_eq!(snapshot.cursor.loaded_blocks, 0);

    controller.splice_transcript(0, entries(0, 1));

    let update = publisher.changes_paged(&controller).unwrap();

    assert_eq!(update.cursor.loaded_blocks, 1);
    assert_eq!(update.cursor.start, 0);
    assert_eq!(update.cursor.end, 1);
}

#[test]
fn pagination_state_survives_json_and_legacy_views_remain_complete() {
    let controller = session(53);

    let mut publisher = ViewPublisher::default();

    let snapshot = publisher.snapshot_paged(&controller);
    let json = serde_json::to_vec(&snapshot).unwrap();
    let decoded: PagedAgentView = serde_json::from_slice(&json).unwrap();

    assert_eq!(snapshot, decoded);
    assert_eq!(publisher.snapshot(&controller).transcript.len(), 53);
}

#[test]
fn edits_in_unloaded_history_do_not_resend_the_loaded_suffix() {
    let controller = session(100);

    let mut publisher = ViewPublisher::default();

    let snapshot = publisher.snapshot_paged(&controller);

    let page = publisher
        .earlier_paged(&controller, snapshot.cursor)
        .unwrap();

    controller
        .conversation()
        .borrow_mut()
        .append_delta("0", "more history", TextField::Reply)
        .unwrap();

    let update = publisher.changes_paged(&controller).unwrap();

    assert_eq!(update.cursor.start, page.cursor.start);
    assert_ne!(update.cursor.revision, page.cursor.revision);
    assert!(
        update
            .ops
            .iter()
            .all(|op| !matches!(op, ViewOp::Splice { .. }))
    );
    assert!(update.reset.is_none());
}

#[test]
fn unloaded_and_live_edits_in_one_batch_publish_only_the_live_suffix() {
    let controller = session(100);

    let mut publisher = ViewPublisher::default();

    publisher.snapshot_paged(&controller);

    let mut conversation = controller.conversation().borrow_mut();

    conversation
        .append_delta("0", "older", TextField::Reply)
        .unwrap();

    conversation
        .append_delta("99", "newer", TextField::Reply)
        .unwrap();

    drop(conversation);

    let update = publisher.changes_paged(&controller).unwrap();

    assert_eq!(splice(&update), (19, 1));
    assert!(update.reset.is_none());
}

#[test]
fn a_large_unloaded_entry_is_not_included_in_the_initial_snapshot() {
    let controller = session(53);

    controller
        .conversation()
        .borrow_mut()
        .append_delta("0", &"x".repeat(9 * 1024 * 1024), TextField::Reply)
        .unwrap();

    let mut publisher = ViewPublisher::default();

    let snapshot = publisher.snapshot_paged(&controller);

    assert_eq!(snapshot.view.transcript.len(), 20);
    assert!(serde_json::to_vec(&snapshot).unwrap().len() < 64 * 1024);
}
