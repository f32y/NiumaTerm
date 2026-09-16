use std::sync::{Arc, mpsc};

use futures::executor::block_on;

use crate::block_store::{BlockStore, SegmentMeta};
use crate::event::{BlockEvent, Msg};
use crate::ghostty::{BlockHandle, GhosttyTerminal};
use crate::pty_pipe::requests::answer_query;
use crate::render_buffer::RenderBuffer;
use crate::selection::SelectionType;
use crate::session::blocks::frozen_selection_pieces;
use crate::session::request::Request;
use crate::session::state_tests::session_from_engine;
use crate::session::{BlockPoint, TerminalSession};

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
            meta: SegmentMeta::default(),
        },
        BlockEvent::EngineBlock {
            seq: 2,
            handle: BlockHandle {
                id: 7,
                generation: 1,
            },
            rows: 5,
            meta: SegmentMeta::default(),
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

fn complete<T>(
    request: Request<T>,
    engine: &mut GhosttyTerminal,
    messages: &mpsc::Receiver<Msg>,
) -> T {
    while let Ok(Msg::Query(query)) = messages.try_recv() {
        answer_query(engine, 0, 0, query);
    }

    block_on(request).unwrap().unwrap()
}

#[test]
fn session_block_reads_copy_expand_and_preserve_row_metadata() {
    let mut engine = GhosttyTerminal::new(24, 4, 100).unwrap();

    engine.write_vt(b"hello world\r\n\x1b]8;;https://example.com\x07linked\x1b]8;;\x07");

    let handle = engine.finish_block().unwrap().unwrap();
    let (session, messages) = session_from_engine(&mut engine);

    session
        .block_store()
        .lock()
        .apply([BlockEvent::EngineBlock {
            seq: 1,
            handle,
            rows: 2,
            meta: SegmentMeta {
                command: Some("echo hello".into()),
                ..SegmentMeta::default()
            },
        }]);

    assert_eq!(session.block_command(0).as_deref(), Some("echo hello"));
    assert_eq!(
        complete(session.block_text(0).unwrap(), &mut engine, &messages),
        "hello world\nlinked"
    );
    assert!(session.block_text(10).is_none());

    let point = BlockPoint {
        item: 0,
        line: 0,
        col: 7,
    };

    let (a, b) = complete(
        session
            .expand_frozen_selection(point, SelectionType::Semantic)
            .unwrap(),
        &mut engine,
        &messages,
    );

    assert_eq!((a.1, b.1), (6, 10));

    let a = BlockPoint {
        item: 0,
        line: a.0,
        col: a.1,
    };

    let b = BlockPoint {
        item: 0,
        line: b.0,
        col: b.1,
    };

    assert_eq!(
        complete(session.frozen_selection_text(b, a), &mut engine, &messages),
        "world"
    );
    assert!(session.block_row_text(0, 1).is_none());

    while let Ok(Msg::Query(query)) = messages.try_recv() {
        answer_query(&mut engine, 0, 0, query);
    }

    let row = session.block_row_text(0, 1).unwrap();

    assert_eq!(row.text.chars().count(), 24);
    assert!(row.text.starts_with("linked"));
    assert!(!row.wrapped);
    assert_eq!(row.hyperlinks, [(0, 5, "https://example.com".into())]);
    assert!(session.block_row_text(9, 0).is_none());
}

#[test]
fn retained_frame_and_history_page_do_not_prevent_reflow() {
    let mut engine = GhosttyTerminal::new(24, 4, 100).unwrap();

    engine.write_vt(b"hello world");

    let handle = engine.finish_block().unwrap().unwrap();
    let (session, messages) = session_from_engine(&mut engine);

    assert!(session.block_page(handle, 0).is_none());

    while let Ok(Msg::Query(query)) = messages.try_recv() {
        answer_query(&mut engine, 0, 0, query);
    }

    let page = session.block_page(handle, 0).unwrap();
    let old = session.snapshot();

    engine.resize(12, 4, 8, 18).unwrap();

    let mut next = engine.snapshot().unwrap();

    session.render_buffer.publish(&mut next);

    assert_eq!(old.cols(), 24);
    assert_eq!(page.cols, 24);
    assert_eq!(session.snapshot().cols(), 12);
    assert_eq!(page.rows[0].cells[0].text.as_str(), "h");
}

fn publish_screen(
    session: &TerminalSession,
    engine: &mut GhosttyTerminal,
    revision: u64,
    theme_revision: u64,
) -> Arc<RenderBuffer> {
    let mut next = RenderBuffer::new(engine.cols() as usize, engine.rows() as usize);

    engine
        .snapshot_into(&mut next, revision, theme_revision)
        .unwrap();

    session.render_buffer.publish(&mut next);

    session.snapshot()
}

fn answer_all(
    engine: &mut GhosttyTerminal,
    revision: u64,
    theme_revision: u64,
    messages: &mpsc::Receiver<Msg>,
) {
    while let Ok(Msg::Query(query)) = messages.try_recv() {
        answer_query(engine, revision, theme_revision, query);
    }
}

/// New output bumps the revision, which misses the page cache. Painting keeps
/// the previous page for those rows until the fresh read lands; exact text
/// reads stay pending; removing history drops the retained page.
#[test]
fn display_page_outlives_its_revision_until_history_is_removed() {
    let mut engine = GhosttyTerminal::new(24, 4, 100).unwrap();

    engine.write_vt(b"one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix\r\n");

    let (session, messages) = session_from_engine(&mut engine);
    let first = session.snapshot();

    assert!(session.screen_page_for_display(&first, 0).is_none());

    answer_all(&mut engine, 0, 0, &messages);

    let page = session.screen_page_for_display(&first, 0).unwrap();

    assert_eq!(page.rows[0].cells[0].text.as_str(), "o");

    engine.write_vt(b"seven\r\n");

    let second = publish_screen(&session, &mut engine, 1, 0);
    let retained = session.screen_page_for_display(&second, 0).unwrap();

    assert!(Arc::ptr_eq(&retained, &page));
    assert!(session.screen_row_text_in(&second, 0).is_none());

    answer_all(&mut engine, 1, 0, &messages);

    let fresh = session.screen_page_for_display(&second, 0).unwrap();

    assert!(!Arc::ptr_eq(&fresh, &page));
    assert_eq!(
        session
            .screen_row_text_in(&second, 0)
            .unwrap()
            .text
            .trim_end(),
        "one"
    );

    session
        .block_store()
        .lock()
        .apply([BlockEvent::HistoryCleared]);

    engine.write_vt(b"eight\r\n");

    let third = publish_screen(&session, &mut engine, 2, 0);

    assert!(session.screen_page_for_display(&third, 0).is_none());
}

/// A retained page carries the width it was wrapped at and colors resolved
/// against the theme of its time, so either change discards it.
#[test]
fn display_page_drops_after_reflow_or_theme_change() {
    let mut engine = GhosttyTerminal::new(24, 4, 100).unwrap();

    engine.write_vt(b"one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix\r\n");

    let (session, messages) = session_from_engine(&mut engine);
    let first = session.snapshot();

    session.screen_page_for_display(&first, 0);

    answer_all(&mut engine, 0, 0, &messages);

    assert!(session.screen_page_for_display(&first, 0).is_some());

    let recolored = publish_screen(&session, &mut engine, 1, 1);

    assert!(session.screen_page_for_display(&recolored, 0).is_none());

    answer_all(&mut engine, 1, 1, &messages);

    assert!(session.screen_page_for_display(&recolored, 0).is_some());

    engine.resize(12, 4, 8, 18).unwrap();

    let narrow = publish_screen(&session, &mut engine, 2, 1);

    assert!(session.screen_page_for_display(&narrow, 0).is_none());
}
