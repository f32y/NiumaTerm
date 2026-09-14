use std::cell::RefCell;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, mpsc};
use std::time::SystemTime;

use nmt_platform::{Poll, Token, Waker};

use crate::event::{BlockEvent, CommandCapture, EventListener, Msg, MsgSender, TerminalEvent};
use crate::ghostty::{BlockHandle, GhosttyTerminal};
use crate::pty_pipe::SessionWorker;
use crate::publication::FrameStore;
use crate::render_buffer::RenderBuffer;
use crate::selection::SelectionType;
use crate::session::mouse::{SurfaceCellSide, SurfaceMouseEventKind, SurfaceScreenCell};
use crate::session::page::PageCache;
use crate::session::proxy::TerminalEventProxy;
use crate::session::{SessionSharedState, TerminalSession};
use crate::terminal::Mode;

#[test]
fn path_paste_and_block_replay_obey_session_input_rules() {
    let (session, messages) = test_session();

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
        }]);

    session
        .block_store()
        .lock()
        .update_meta(1, |meta| meta.command = Some("echo hello".into()));

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

pub(super) fn test_session() -> (TerminalSession, mpsc::Receiver<Msg>) {
    let mut engine = GhosttyTerminal::new(24, 4, 100).unwrap();

    engine.write_vt(b"hello world");

    session_from_engine(&mut engine)
}

pub(super) fn session_from_engine(
    engine: &mut GhosttyTerminal,
) -> (TerminalSession, mpsc::Receiver<Msg>) {
    let (tx, rx) = mpsc::channel();
    let poll = Poll::new().unwrap();
    let waker = Arc::new(Waker::new(poll.registry(), Token(0)).unwrap());

    let mut buffer = RenderBuffer::new(engine.cols() as usize, engine.rows() as usize);

    engine.snapshot_into(&mut buffer, 0, 0).unwrap();

    let messenger = MsgSender::new(tx, waker);

    (
        TerminalSession {
            _worker: SessionWorker::without_thread_for_test(messenger.clone()),
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
    let (session, rx) = test_session();

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
    let (mut session, rx) = test_session();

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
    let (session, rx) = test_session();
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

        engine.poll_pwd();

        let mut snapshot = engine.snapshot().unwrap();

        session.render_buffer.publish(&mut snapshot);

        assert_eq!(session.current_directory().as_deref(), Some(expected));
    }
}
