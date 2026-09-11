use std::sync::atomic::AtomicU32;
use std::sync::{Arc, mpsc};
use std::time::SystemTime;

use nmt_platform::{Poll, Token, Waker};
use parking_lot::FairMutex;

use crate::event::{CommandCapture, EventListener, Msg, MsgSender, TerminalEvent, WindowId};
use crate::ghostty::GhosttyTerminal;
use crate::render_buffer::RenderBuffer;
use crate::selection::SelectionType;
use crate::session::mouse::{SurfaceCellSide, SurfaceMouseEventKind, SurfaceScreenCell};
use crate::session::proxy::TerminalEventProxy;
use crate::session::{SessionSharedState, TerminalSession};

pub(super) fn test_session() -> (TerminalSession, mpsc::Receiver<Msg>) {
    let (tx, rx) = mpsc::channel();
    let poll = Poll::new().unwrap();
    let waker = Arc::new(Waker::new(poll.registry(), Token(0)).unwrap());
    let mut engine = GhosttyTerminal::new(24, 4, 100).unwrap();
    engine.write_vt(b"hello world");
    let mut buffer = RenderBuffer::new(24, 4);
    engine.snapshot_into(&mut buffer).unwrap();

    (
        TerminalSession {
            engine: Arc::new(FairMutex::new(engine)),
            render_buffer: Arc::new(FairMutex::new(buffer)),
            vt_modes: Arc::new(AtomicU32::new(0)),
            messenger: MsgSender::new(tx, waker),
            shared: Arc::new(SessionSharedState::default()),
            process_tree: None,
            engine_blocks: false,
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
fn runtime_transitions_do_not_require_host_event_consumption() {
    let (session, rx) = test_session();
    let proxy = TerminalEventProxy::new(session.shared.clone(), 1, None);
    let window = WindowId::dummy();

    proxy.send_event(TerminalEvent::AltScreen(true), window);
    assert!(session.alt_screen());
    proxy.send_event(TerminalEvent::AltScreen(false), window);
    assert!(!session.alt_screen());

    session.apply_screen_selection(
        SurfaceScreenCell { row: 0, col: 1 },
        SurfaceCellSide::Left,
        SurfaceMouseEventKind::Down,
        SelectionType::Semantic,
    );
    assert!(session.selection_range().is_some());

    let now = SystemTime::now();
    proxy.send_event(
        TerminalEvent::CommandFinished(CommandCapture {
            seq: 1,
            command: "echo hello".into(),
            cwd: None,
            exit_code: Some(0),
            started_at: now,
            ended_at: now,
        }),
        window,
    );
    assert!(session.selection_range().is_none());

    proxy.send_event(TerminalEvent::CloseTerminal(0), window);
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
        session
            .engine
            .lock()
            .write_vt(format!("\x1b]7;{reported}\x07").as_bytes());
        assert_eq!(session.current_directory().as_deref(), Some(expected));
    }
}
