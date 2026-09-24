use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::SystemTime;

use futures::executor::block_on;
use tokio::sync::mpsc;

use crate::block_store::{BlockStore, SegmentMeta};
use crate::event::{
    BlockEvent, CommandCapture, EventListener, Msg, MsgSender, Request, TerminalEvent,
};
use crate::ghostty::{BlockHandle, GhosttyTerminal};
use crate::render_buffer::{FrameStore, RenderBuffer};
use crate::selection::SelectionType;
use crate::session::page::PageCache;
use crate::session::proxy::TerminalEventProxy;
use crate::session::selection::frozen_selection_pieces;
use crate::session::{
    BlockPoint, SessionSharedState, SurfaceCellSide, SurfaceMouseEventKind, SurfaceScreenCell,
    TerminalSession,
};
use crate::termio::SessionWorker;
use crate::termio::requests::answer_query;
use crate::vt_modes::Mode;

#[test]
fn path_paste_and_block_replay_obey_session_input_rules() {
    let (session, mut messages) = test_session();

    session
        .vt_modes
        .store(Mode::BRACKETED_PASTE.bits(), Ordering::Release);

    let paths = [r"C:\src\main.rs".into(), r"C:\My Project\notes.txt".into()];

    assert!(!session.paste_paths(&[]));
    assert!(session.paste_paths(&paths));
    assert!(matches!(messages.try_recv().unwrap(), Msg::Input(bytes)
        if bytes.as_ref() == b"\x1b[200~C:\\src\\main.rs \"C:\\My Project\\notes.txt\"\x1b[201~"));

    session
        .block_store()
        .lock()
        .apply([BlockEvent::EngineBlock {
            seq: 1,
            handle: BlockHandle {
                id: 1,
                generation: 1,
            },
            rows: 1,
            meta: SegmentMeta {
                command: Some("echo hello".into()),
                ..SegmentMeta::default()
            },
        }]);

    assert!(session.rerun_block(0));
    assert!(
        matches!(messages.try_recv().unwrap(), Msg::Input(bytes) if bytes.as_ref() == b"echo hello\r")
    );
    assert!(!session.rerun_block(99));

    session.mark_read_only();

    assert!(!session.paste_paths(&paths));
    assert!(!session.rerun_block(0));
    assert!(messages.try_recv().is_err());
}

pub(super) fn test_session() -> (TerminalSession, mpsc::UnboundedReceiver<Msg>) {
    let mut engine = GhosttyTerminal::new(24, 4, 100).unwrap();

    engine.write_vt(b"hello world");

    session_from_engine(&mut engine)
}

pub(super) fn session_from_engine(
    engine: &mut GhosttyTerminal,
) -> (TerminalSession, mpsc::UnboundedReceiver<Msg>) {
    let (tx, rx) = mpsc::unbounded_channel();

    let mut buffer = RenderBuffer::new(engine.cols() as usize, engine.rows() as usize);

    engine.snapshot_into(&mut buffer, 0, 0).unwrap();

    let messenger: MsgSender = tx;

    (
        TerminalSession {
            _worker: SessionWorker::detached_for_test(messenger.clone()),
            pages: RefCell::new(PageCache::default()),
            render_buffer: Arc::new(FrameStore::new(buffer)),
            vt_modes: Arc::new(AtomicU32::new(0)),
            messenger,
            shared: Arc::new(SessionSharedState::default()),
            process_tree: None,
            engine_blocks: false,
            supports_powershell_compatibility: false,
        },
        rx,
    )
}

#[test]
fn writes_report_queue_acceptance_and_reject_closed_or_read_only_sessions() {
    let (session, mut rx) = test_session();

    assert!(!session.write_input(b""));
    assert!(session.write_text("hello"));
    assert!(matches!(rx.try_recv().unwrap(), Msg::Input(bytes) if bytes.as_ref() == b"hello"));

    session.mark_read_only();

    assert!(!session.write_text("ignored"));
    assert!(!session.paste_text("ignored"));
    assert!(rx.try_recv().is_err());

    let (closed, receiver) = test_session();

    drop(receiver);

    assert!(!closed.write_text("undeliverable"));
    assert!(!closed.resize(80, 24, 800, 480));
}

#[test]
fn powershell_setting_only_updates_supported_sessions_and_can_be_disabled() {
    let (mut session, mut rx) = test_session();

    assert!(!session.set_powershell_compatibility(true));
    assert!(rx.try_recv().is_err());

    session.supports_powershell_compatibility = true;

    assert!(session.set_powershell_compatibility(true));
    assert!(session.set_powershell_compatibility(false));
    assert!(matches!(
        rx.try_recv().unwrap(),
        Msg::PowerShellCompatibility(true)
    ));
    assert!(matches!(
        rx.try_recv().unwrap(),
        Msg::PowerShellCompatibility(false)
    ));
}

#[test]
fn runtime_transitions_do_not_require_host_event_consumption() {
    let (session, mut rx) = test_session();

    let proxy = TerminalEventProxy::new(session.shared.clone(), 1, None);

    proxy.send_event(TerminalEvent::AltScreen(true));

    assert!(session.alt_screen());

    proxy.send_event(TerminalEvent::AltScreen(false));

    assert!(!session.alt_screen());

    session.apply_screen_selection(
        SurfaceScreenCell { row: 0, col: 1 },
        SurfaceCellSide::Left,
        SurfaceMouseEventKind::Down,
        SelectionType::Semantic,
    );

    assert!(session.selection_range_in(&session.snapshot()).is_some());

    let now = SystemTime::now();

    proxy.send_event(TerminalEvent::CommandFinished(CommandCapture {
        seq: 1,
        command: Some("echo hello".into()),
        cwd: None,
        exit_code: Some(0),
        started_at: now,
        ended_at: now,
    }));

    assert!(session.selection_range_in(&session.snapshot()).is_none());

    proxy.send_event(TerminalEvent::CloseTerminal(0));

    assert!(session.exited());
    assert!(!session.write_text("after exit"));
    assert!(!session.resize(80, 24, 800, 480));
    assert!(rx.try_recv().is_err());
    assert!(!session.shared.events.lock().is_empty());
}

#[test]
fn current_directory_is_available_before_event_drain() {
    let (session, _) = test_session();

    for (reported, expected) in [
        ("file:///C:/Projects/example", "C:/Projects/example"),
        ("file://host/home/u", "/home/u"),
        (r"C:\plain\path", r"C:\plain\path"),
        ("/unix/path", "/unix/path"),
    ] {
        let mut engine = GhosttyTerminal::new(24, 4, 100).unwrap();

        engine.write_vt(format!("\x1b]7;{reported}\x07").as_bytes());

        let mut snapshot = engine.snapshot().unwrap();

        session.render_buffer.publish(&mut snapshot);

        assert_eq!(session.current_directory().as_deref(), Some(expected));
    }
}

// ---------------------------------------------------------------------------
// Frozen blocks, pages, and retained display pages
// ---------------------------------------------------------------------------

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
    messages: &mut mpsc::UnboundedReceiver<Msg>,
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

    let (session, mut messages) = session_from_engine(&mut engine);

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
        complete(session.block_text(0).unwrap(), &mut engine, &mut messages),
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
        &mut messages,
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
        complete(
            session.frozen_selection_text(b, a),
            &mut engine,
            &mut messages
        ),
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

    let (session, mut messages) = session_from_engine(&mut engine);

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
    messages: &mut mpsc::UnboundedReceiver<Msg>,
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

    let (session, mut messages) = session_from_engine(&mut engine);

    let first = session.snapshot();

    assert!(session.screen_page_for_display(&first, 0).is_none());

    answer_all(&mut engine, 0, 0, &mut messages);

    let page = session.screen_page_for_display(&first, 0).unwrap();

    assert_eq!(page.rows[0].cells[0].text.as_str(), "o");

    engine.write_vt(b"seven\r\n");

    let second = publish_screen(&session, &mut engine, 1, 0);
    let retained = session.screen_page_for_display(&second, 0).unwrap();

    assert!(Arc::ptr_eq(&retained, &page));
    assert!(session.screen_row_text_in(&second, 0).is_none());

    answer_all(&mut engine, 1, 0, &mut messages);

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

    let (session, mut messages) = session_from_engine(&mut engine);

    let first = session.snapshot();

    session.screen_page_for_display(&first, 0);

    answer_all(&mut engine, 0, 0, &mut messages);

    assert!(session.screen_page_for_display(&first, 0).is_some());

    let recolored = publish_screen(&session, &mut engine, 1, 1);

    assert!(session.screen_page_for_display(&recolored, 0).is_none());

    answer_all(&mut engine, 1, 1, &mut messages);

    assert!(session.screen_page_for_display(&recolored, 0).is_some());

    engine.resize(12, 4, 8, 18).unwrap();

    let narrow = publish_screen(&session, &mut engine, 2, 1);

    assert!(session.screen_page_for_display(&narrow, 0).is_none());
}
