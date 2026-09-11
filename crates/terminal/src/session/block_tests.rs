use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use crate::block_store::BlockStore;
use crate::event::BlockEvent;
use crate::ghostty::BlockHandle;
use crate::selection::SelectionType;
use crate::session::BlockPoint;
use crate::session::blocks::frozen_selection_pieces;
use crate::session::state_tests::test_session;

/// `frozen_selection_pieces` produces one per-block range with block-edge
/// endpoints resolved per item.
#[test]
fn selection_pieces_cover_block_ranges() {
    let mut store = BlockStore::default();

    store.apply([
        BlockEvent::EngineBlock {
            seq: 1,
            handle: BlockHandle {
                id: 6,
                generation: 1,
            },
            rows: 2,
        },
        BlockEvent::EngineBlock {
            seq: 2,
            handle: BlockHandle {
                id: 7,
                generation: 1,
            },
            rows: 5,
        },
    ]);

    let pieces = frozen_selection_pieces(
        &store,
        BlockPoint {
            item: 0,
            line: 0,
            col: 2,
        },
        BlockPoint {
            item: 1,
            line: 3,
            col: 4,
        },
    );

    assert_eq!(pieces.len(), 2);
    assert_eq!(pieces[0].handle.id, 6);
    assert_eq!(pieces[0].start, Some((0, 2)));
    assert_eq!(pieces[0].end, None, "selection continues past this item");
    assert_eq!(pieces[1].handle.id, 7);
    assert_eq!(pieces[1].start, None, "selection starts before this item");
    assert_eq!(pieces[1].end, Some((3, 4)));
}

#[test]
fn session_block_reads_copy_expand_and_preserve_row_metadata() {
    let (session, _rx) = test_session();
    let handle = {
        let mut engine = session.engine.lock();
        engine.write_vt(b"\r\n\x1b]8;;https://example.com\x07linked\x1b]8;;\x07");
        engine.finish_block().unwrap().unwrap()
    };
    session
        .block_store()
        .lock()
        .apply([BlockEvent::EngineBlock {
            seq: 1,
            handle,
            rows: 2,
        }]);
    session
        .block_store()
        .lock()
        .update_meta(1, |meta| meta.command = Some("echo hello".into()));

    assert_eq!(session.block_command(0).as_deref(), Some("echo hello"));
    assert_eq!(
        session.block_text(0).as_deref(),
        Some("hello world\nlinked")
    );
    assert_eq!(session.block_text(10), None);
    let point = BlockPoint {
        item: 0,
        line: 0,
        col: 7,
    };
    let (a, b) = session
        .expand_frozen_selection(point, SelectionType::Semantic)
        .unwrap();
    assert_eq!((a.col, b.col), (6, 10));
    assert_eq!(session.frozen_selection_text(b, a), "world");
    let (a, b) = session
        .expand_frozen_selection(point, SelectionType::Lines)
        .unwrap();
    assert_eq!(session.frozen_selection_text(a, b), "hello world");

    let row = session.block_row_text(0, 1).unwrap();
    assert_eq!(row.text.chars().count(), 24);
    assert!(row.text.starts_with("linked"));
    assert!(!row.wrapped);
    assert_eq!(row.hyperlinks, [(0, 5, "https://example.com".into())]);
    assert!(session.block_row_text(9, 0).is_none());
    session.engine.lock().write_vt(b"new row");
    assert!(
        session
            .screen_row_text(0)
            .unwrap()
            .text
            .starts_with("new row")
    );
}

#[test]
fn block_reads_release_store_while_waiting_for_engine() {
    let (session, _rx) = test_session();
    let handle = session.engine.lock().finish_block().unwrap().unwrap();
    session
        .block_store()
        .lock()
        .apply([BlockEvent::EngineBlock {
            seq: 1,
            handle,
            rows: 1,
        }]);
    let session = Arc::new(session);
    let (started_tx, started_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let engine = session.engine.lock();
    let reader = Arc::clone(&session);
    thread::spawn(move || {
        started_tx.send(()).unwrap();
        let text = reader.block_text(0);
        done_tx.send(text).unwrap();
    });
    started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    // Give the reader a chance to reach the held engine while checking the
    // worker's engine-then-store acquisition remains possible.
    thread::sleep(Duration::from_millis(20));
    let store = session.block_store();
    assert!(store.try_lock_for(Duration::from_secs(1)).is_some());
    drop(engine);
    assert_eq!(
        done_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .as_deref(),
        Some("hello world")
    );
}
