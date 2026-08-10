use std::sync::Arc;
use std::sync::atomic::AtomicU32;
use std::{io, sync, time};

use nmt_config::colors::Colors;
use nmt_platform::{ChildEvent, EventedPty, ProcessReadWrite, WinsizeBuilder};
use parking_lot::{FairMutex, Mutex};

use crate::event::{self, VoidListener};
use crate::pty_pipe::{
    Interest, Poll, PtyPipe, PtyState, READ_BUFFER_SIZE, SYNC_OUTPUT_TIMEOUT, Token, Waker,
    max_cup_row_col, mode, publish_render_buffer, rewrite_conpty_resize_echo_cup_rows,
    su_realign_count,
};
use crate::render_buffer::RenderBuffer;
use crate::{ansi, ghostty};

#[test]
fn failed_capture_does_not_publish_back_buffer() {
    let front = FairMutex::new(RenderBuffer::new(2, 1));
    let mut back = RenderBuffer::new(3, 1);

    assert!(!publish_render_buffer(
        &front,
        &mut back,
        Err(ghostty::Error::InvalidValue),
        false,
    ));
    assert_eq!(front.lock().cols(), 2);
    assert_eq!(back.cols(), 3);

    assert!(publish_render_buffer(&front, &mut back, Ok(()), false));
    assert_eq!(front.lock().cols(), 3);
    assert_eq!(back.cols(), 2);
}

fn snapshot_row_text(snapshot: &RenderBuffer, y: u16) -> String {
    render_buffer_row_text(snapshot, y as usize)
}

fn render_buffer_row_text(buffer: &RenderBuffer, y: usize) -> String {
    (0..buffer.cols())
        .map(|x| {
            let c = buffer.cell(x, y).c();
            if c == '\0' { ' ' } else { c }
        })
        .collect::<String>()
        .trim_end()
        .to_string()
}

#[test]
fn conpty_resize_echo_rewrites_cup_to_engine_cursor_row() {
    let rewritten = rewrite_conpty_resize_echo_cup_rows(b"\x1b[10;18Hx\x1b[10;19H", 42)
        .expect("CUP row should be rewritten");
    assert_eq!(rewritten, b"\x1b[42;18Hx\x1b[42;19H");
}

#[test]
fn su_realign_count_computes_push_to_align_prompt() {
    // Ghostty keeps the prompt on row 17 while ConPTY repaints it on row 1;
    // (CUP row 2, last CUP). N = 17 − 1 = 16, R_conpty = 1. Repaint fits 80x24.
    let repaint = b"\x1b[2;18Hdir\x1b[2;21H";
    assert_eq!(su_realign_count(repaint, 17, 80, 24, 80, 24), Some((16, 1)));
}

#[test]
fn su_realign_count_uses_last_cup_for_multirow_prompt() {
    // Multi-row wrapped prompt: first CUP row 10 (top), last CUP row 11 (cursor).
    // Anchoring on the LAST CUP (row 11 → R_conpty 10) matches R_ghostty (cursor
    // row 22), so N = 22 − 10 = 12 — not 13, which the first CUP (row 10) would give.
    let repaint = b"\x1b[10;22Hxxx\x1b[11;6H";
    assert_eq!(
        su_realign_count(repaint, 22, 90, 40, 90, 40),
        Some((12, 10))
    );
}

#[test]
fn su_realign_count_none_when_already_aligned() {
    // R_ghostty == R_conpty means no divergence and no SU; the legacy CUP rewrite handles it.
    let repaint = b"\x1b[2;18Hdir\x1b[2;21H";
    assert_eq!(su_realign_count(repaint, 1, 80, 24, 80, 24), None);
}

#[test]
fn su_realign_count_none_on_resize_mismatch() {
    let repaint = b"\x1b[2;18Hdir\x1b[2;21H";
    // engine size moved past the latch → repaint answers a different resize.
    assert_eq!(su_realign_count(repaint, 17, 80, 24, 100, 24), None);
    // repaint addresses col 90, beyond the latched 80 cols → different resize.
    let wide = b"\x1b[2;90Hx\x1b[2;90H";
    assert_eq!(su_realign_count(wide, 17, 80, 24, 80, 24), None);
}

#[test]
fn su_realign_count_none_without_cup() {
    assert_eq!(su_realign_count(b"\x1b[?25l", 17, 80, 24, 80, 24), None);
}

#[test]
fn max_cup_row_col_reports_maxima() {
    assert_eq!(max_cup_row_col(b"\x1b[2;18Hx\x1b[11;6H"), (11, 18));
    assert_eq!(max_cup_row_col(b"no cup here"), (0, 0));
}

#[test]
fn conpty_resize_echo_leaves_wrapped_redraw_when_cursor_already_aligned() {
    // Narrow width: PSReadLine redraws the input across two rows — input starts
    // at row 10, wraps, cursor ends at row 11. ConPTY's cursor row (11) already
    // matches ghostty's cursor row (target 11), so the redraw must be left
    // untouched. Collapsing the first CUP onto row 11 re-wraps the content and
    // makes it accumulate (the cols=37 xxxx-duplication bug).
    let wrapped = b"\x1b[10;22Hxxxxxxxxxxxxxxxxxxxxx\x1b[11;6H";
    assert_eq!(rewrite_conpty_resize_echo_cup_rows(wrapped, 11), None);
}

#[test]
fn conpty_resize_echo_shifts_wrapped_redraw_by_delta_preserving_rows() {
    // Divergent multi-row redraw: ConPTY input spans rows 9-10 (cursor row 10),
    // ghostty's cursor is at row 22. The whole redraw shifts down by 12, keeping
    // the two-row structure intact.
    let rewritten = rewrite_conpty_resize_echo_cup_rows(b"\x1b[9;22Hxxx\x1b[10;6H", 22)
        .expect("divergent redraw should shift");
    assert_eq!(rewritten, b"\x1b[21;22Hxxx\x1b[22;6H");
}

struct FakeReader {
    data: Vec<u8>,
}

impl io::Read for FakeReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.data.is_empty() {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let n = self.data.len().min(buf.len());
        buf[..n].copy_from_slice(&self.data[..n]);
        self.data.drain(..n);
        Ok(n)
    }
}

#[derive(Default)]
struct FakeWriter {
    data: Vec<u8>,
}

impl io::Write for FakeWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.data.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct FakePty {
    reader: FakeReader,
    writer: FakeWriter,
}

impl ProcessReadWrite for FakePty {
    type Reader = FakeReader;
    type Writer = FakeWriter;

    fn reader(&mut self) -> &mut Self::Reader {
        &mut self.reader
    }

    fn read_token(&self) -> Token {
        Token(1)
    }

    fn writer(&mut self) -> &mut Self::Writer {
        &mut self.writer
    }

    fn write_token(&self) -> Token {
        Token(2)
    }

    fn set_winsize(&mut self, _: WinsizeBuilder) -> io::Result<()> {
        Ok(())
    }

    fn register(
        &mut self,
        _: &Poll,
        _: &mut dyn Iterator<Item = Token>,
        _: Interest,
        _: &sync::Arc<Waker>,
    ) -> io::Result<()> {
        Ok(())
    }

    fn reregister(&mut self, _: &Poll, _: Interest) -> io::Result<()> {
        Ok(())
    }

    fn deregister(&mut self, _: &Poll) -> io::Result<()> {
        Ok(())
    }

    fn drain_ready(&self) -> Vec<Token> {
        Vec::new()
    }
}

impl EventedPty for FakePty {
    fn child_event_token(&self) -> Token {
        Token(3)
    }

    fn next_child_event(&mut self) -> Option<ChildEvent> {
        None
    }
}

#[test]
fn disabled_terminal_responses_are_forwarded_without_replying() {
    let queries = b"\x1b]10;?\x1b\\\x1b]11;?\x1b\\\x1b[c";
    let pty = FakePty {
        reader: FakeReader {
            data: queries.to_vec(),
        },
        writer: FakeWriter::default(),
    };
    let mut machine = PtyPipe::new(
        Arc::new(FairMutex::new(RenderBuffer::new(20, 3))),
        Arc::new(AtomicU32::new(0)),
        pty,
        VoidListener {},
        event::WindowId::from(0),
        0,
        Colors::default(),
        1000,
        false,
    )
    .unwrap();
    let forwarded = Arc::new(Mutex::new(Vec::new()));
    let forwarded_sink = Arc::clone(&forwarded);
    machine.set_output_sink(move |bytes| forwarded_sink.lock().extend_from_slice(&bytes));
    machine.set_terminal_responses_enabled(false);

    machine
        .pty_read(&mut PtyState::default(), &mut [0; READ_BUFFER_SIZE])
        .unwrap();

    assert_eq!(&*forwarded.lock(), queries);
    assert!(machine.pty.writer.data.is_empty());
}

#[test]
fn resize_message_publishes_snapshot_to_render_buffer() {
    let render_buffer = Arc::new(FairMutex::new(RenderBuffer::new(20, 3)));
    let vt_modes = Arc::new(AtomicU32::new(0));
    let pty = FakePty {
        reader: FakeReader { data: Vec::new() },
        writer: FakeWriter::default(),
    };
    let mut machine = PtyPipe::new(
        Arc::clone(&render_buffer),
        vt_modes,
        pty,
        VoidListener {},
        event::WindowId::from(0),
        0,
        Colors::default(),
        1000,
        false,
    )
    .unwrap();

    {
        let mut engine = machine.ghostty.lock();
        engine.write_vt(b"resize-ok");
    }

    machine
        .sender
        .send(event::Msg::Resize(WinsizeBuilder {
            rows: 5,
            cols: 20,
            width: 240,
            height: 120,
        }))
        .unwrap();

    let mut state = PtyState::default();
    assert!(machine.drain_recv_channel(&mut state));

    let buffer = render_buffer.lock();
    assert_eq!(buffer.rows(), 5);
    assert_eq!(render_buffer_row_text(&buffer, 0), "resize-ok");
}

#[test]
fn synchronized_output_keeps_published_cursor_on_previous_frame_until_commit() {
    let render_buffer = Arc::new(FairMutex::new(RenderBuffer::new(20, 3)));
    let pty = FakePty {
        reader: FakeReader {
            data: b"\x1b[3;1H> input\x1b[3;3H".to_vec(),
        },
        writer: FakeWriter::default(),
    };
    let mut machine = PtyPipe::new(
        Arc::clone(&render_buffer),
        Arc::new(AtomicU32::new(0)),
        pty,
        VoidListener {},
        event::WindowId::from(0),
        0,
        Colors::default(),
        1000,
        false,
    )
    .unwrap();
    let mut state = PtyState::default();
    let mut buf = [0u8; READ_BUFFER_SIZE];

    machine.pty_read(&mut state, &mut buf).unwrap();
    assert_eq!(render_buffer.lock().cursor().row.0, 2);

    machine
        .pty
        .reader
        .data
        .extend_from_slice(b"\x1b[?2026h\x1b[1;1HWorking");
    machine.pty_read(&mut state, &mut buf).unwrap();
    {
        let buffer = render_buffer.lock();
        assert_eq!(
            buffer.cursor().row.0,
            2,
            "an incomplete synchronized frame must not expose its intermediate cursor"
        );
        assert!(
            render_buffer_row_text(&buffer, 0).is_empty(),
            "an incomplete synchronized frame must not expose its intermediate cells"
        );
    }

    machine
        .pty
        .reader
        .data
        .extend_from_slice(b"\x1b[3;3H\x1b[?2026l");
    machine.pty_read(&mut state, &mut buf).unwrap();
    {
        let buffer = render_buffer.lock();
        assert_eq!(buffer.cursor().row.0, 2);
        assert_eq!(render_buffer_row_text(&buffer, 0), "Working");
    }

    machine
        .pty
        .reader
        .data
        .extend_from_slice(b"\x1b[?2026h\x1b[2;1HStuck");
    machine.pty_read(&mut state, &mut buf).unwrap();
    machine.sync_output_started_at = Some(time::Instant::now() - SYNC_OUTPUT_TIMEOUT);
    machine.pty_read(&mut state, &mut buf).unwrap();

    {
        let buffer = render_buffer.lock();
        assert_eq!(buffer.cursor().row.0, 1);
        assert_eq!(render_buffer_row_text(&buffer, 1), "Stuck");
    }
    assert!(!machine.ghostty.lock().mode(mode::SYNC_OUTPUT));
}

#[test]
fn osc_progress_hides_published_cursor_until_removed() {
    let render_buffer = Arc::new(FairMutex::new(RenderBuffer::new(80, 3)));
    let pty = FakePty {
        reader: FakeReader {
            data: b"\x1b]9;4;1".to_vec(),
        },
        writer: FakeWriter::default(),
    };
    let mut machine = PtyPipe::new(
        Arc::clone(&render_buffer),
        Arc::new(AtomicU32::new(0)),
        pty,
        VoidListener {},
        event::WindowId::from(0),
        0,
        Colors::default(),
        1000,
        false,
    )
    .unwrap();
    machine
        .ghostty
        .lock()
        .set_default_cursor_shape(ansi::CursorShape::Beam)
        .unwrap();
    let mut state = PtyState::default();
    let mut buf = [0u8; READ_BUFFER_SIZE];

    machine.pty_read(&mut state, &mut buf).unwrap();
    machine
        .pty
        .reader
        .data
        .extend_from_slice(b";42\x1b\\    Building [====>     ] 4/10\r");
    machine.pty_read(&mut state, &mut buf).unwrap();
    {
        let buffer = render_buffer.lock();
        assert_eq!(buffer.cursor_shape(), ansi::CursorShape::Beam);
        assert!(!buffer.cursor_visible(), "active progress hides the cursor");
    }

    machine
        .pty
        .reader
        .data
        .extend_from_slice(b"\x1b]9;4;0;\x1b\\");
    machine.pty_read(&mut state, &mut buf).unwrap();
    assert!(
        render_buffer.lock().cursor_visible(),
        "removing progress restores the terminal cursor state"
    );
}

#[test]
fn conpty_resize_echo_realigns_machine_pty_read_to_cursor_row() {
    let render_buffer = Arc::new(FairMutex::new(RenderBuffer::new(134, 42)));
    let vt_modes = Arc::new(AtomicU32::new(0));
    let pty = FakePty {
        reader: FakeReader {
            data: b"\x1b[10;24Hx\x1b[10;25H".to_vec(),
        },
        writer: FakeWriter::default(),
    };
    let mut machine = PtyPipe::new(
        render_buffer,
        vt_modes,
        pty,
        VoidListener {},
        event::WindowId::from(0),
        0,
        Colors::default(),
        1000,
        false,
    )
    .unwrap();

    {
        let mut engine = machine.ghostty.lock();
        engine.write_vt(b"\x1b[2J\x1b[10;1HHISTORY\x1b[42;1HC:\\Workspace\\NiumaTerm>");
    }
    machine.conpty_resize_echo_realign = true;
    machine.conpty_resize_echo_pending = true;

    let mut state = PtyState::default();
    let mut read_buf = [0u8; READ_BUFFER_SIZE];
    machine.pty_read(&mut state, &mut read_buf).unwrap();

    let snapshot = machine.ghostty.lock().snapshot().unwrap();
    assert_eq!(snapshot_row_text(&snapshot, 9), "HISTORY");
    assert_eq!(
        snapshot_row_text(&snapshot, 41),
        "C:\\Workspace\\NiumaTerm>x"
    );
}

#[test]
fn conpty_resize_repaint_realigns_clear_without_new_input() {
    let render_buffer = Arc::new(FairMutex::new(RenderBuffer::new(134, 42)));
    let vt_modes = Arc::new(AtomicU32::new(0));
    let pty = FakePty {
        reader: FakeReader {
            data: b"\x1b[1;24H\x1b[J\x1b[1;24HTERMINPUT123\x1b[1;36H".to_vec(),
        },
        writer: FakeWriter::default(),
    };
    let mut machine = PtyPipe::new(
        render_buffer,
        vt_modes,
        pty,
        VoidListener {},
        event::WindowId::from(0),
        0,
        Colors::default(),
        1000,
        false,
    )
    .unwrap();

    {
        let mut engine = machine.ghostty.lock();
        engine.write_vt(b"\x1b[2J\x1b[10;1HHISTORY\x1b[42;1HC:\\Workspace\\NiumaTerm>");
    }
    machine.conpty_resize_echo_realign = true;
    machine.conpty_resize_repaint_reads_remaining = 1;

    let mut state = PtyState::default();
    let mut read_buf = [0u8; READ_BUFFER_SIZE];
    machine.pty_read(&mut state, &mut read_buf).unwrap();

    let snapshot = machine.ghostty.lock().snapshot().unwrap();
    assert_eq!(snapshot_row_text(&snapshot, 9), "HISTORY");
    assert_eq!(
        snapshot_row_text(&snapshot, 41),
        "C:\\Workspace\\NiumaTerm>TERMINPUT123"
    );
}

/// When the viewport is scrolled into history, the resize repaint is still
/// realigned to the ENGINE's active cursor row via `active_cursor_row()` (which is
/// independent of the scroll pin), so it lands on the active prompt row — not on
/// the visible history row, and not forced to row 1 (the old 错位 bug, which came
/// from the viewport-relative `cursor.y` reading 0 while scrolled).
#[test]
fn conpty_resize_repaint_realigns_to_active_cursor_when_scrolled() {
    let render_buffer = Arc::new(FairMutex::new(RenderBuffer::new(20, 4)));
    let vt_modes = Arc::new(AtomicU32::new(0));
    let pty = FakePty {
        // ConPTY targets active row 3 (its own coords). Differs from the
        // off-screen-cursor target row 1, so the old code would rewrite 3 -> 1.
        reader: FakeReader {
            data: b"\x1b[3;1H\x1b[JINJECT\x1b[3;7H".to_vec(),
        },
        writer: FakeWriter::default(),
    };
    let mut machine = PtyPipe::new(
        render_buffer,
        vt_modes,
        pty,
        VoidListener {},
        event::WindowId::from(0),
        0,
        Colors::default(),
        1000,
        false,
    )
    .unwrap();

    {
        let mut engine = machine.ghostty.lock();
        // Fill well past the 4-row viewport so there is scrollback to pin to.
        engine.write_vt(b"\x1b[2JL0\r\nL1\r\nL2\r\nL3\r\nL4\r\nL5\r\nL6\r\nL7\r\nL8\r\nPROMPT>");
        // Pin the viewport to the top of history: cursor now off-screen.
        engine.scroll_viewport_top();
        let sb = engine.snapshot().unwrap().scrollbar();
        assert!(
            sb.offset < sb.total.saturating_sub(sb.len),
            "precondition: viewport must be scrolled away from the bottom"
        );
    }
    machine.conpty_resize_echo_realign = true;
    machine.conpty_resize_repaint_reads_remaining = 1;

    let mut state = PtyState::default();
    let mut read_buf = [0u8; READ_BUFFER_SIZE];
    machine.pty_read(&mut state, &mut read_buf).unwrap();

    // INJECT must be realigned onto the engine's active cursor row (read
    // independently of the scroll pin), exactly once, never on row 0.
    let (active_row, snapshot) = {
        let mut engine = machine.ghostty.lock();
        let active_row = engine.active_cursor_row().unwrap();
        engine.scroll_viewport_bottom();
        (active_row, engine.snapshot().unwrap())
    };
    assert_eq!(snapshot_row_text(&snapshot, active_row), "INJECT");
    assert_ne!(snapshot_row_text(&snapshot, 0), "INJECT");
    let injects = (0..snapshot.rows() as u16)
        .filter(|&y| snapshot_row_text(&snapshot, y).contains("INJECT"))
        .count();
    assert_eq!(injects, 1, "INJECT must appear exactly once");
}

/// Typing while scrolled up: a ConPTY echo of user input is realigned to the
/// ENGINE's active cursor row (`active_cursor_row()`, independent of the viewport
/// scroll), so the echo lands on the active prompt row — never on the scrolled-away
/// history currently in view. ConPTY emits the echo at its own stale CUP row.
#[test]
fn conpty_resize_echo_routes_to_active_cursor_when_scrolled_typing() {
    let render_buffer = Arc::new(FairMutex::new(RenderBuffer::new(20, 4)));
    let vt_modes = Arc::new(AtomicU32::new(0));
    let pty = FakePty {
        // ConPTY echoes the typed 'Z' at its stale row 2, col 8 (after the
        // prompt in ConPTY's frame). The engine's real prompt is elsewhere.
        reader: FakeReader {
            data: b"\x1b[2;8HZ\x1b[2;9H".to_vec(),
        },
        writer: FakeWriter::default(),
    };
    let mut machine = PtyPipe::new(
        render_buffer,
        vt_modes,
        pty,
        VoidListener {},
        event::WindowId::from(0),
        0,
        Colors::default(),
        1000,
        false,
    )
    .unwrap();

    {
        let mut engine = machine.ghostty.lock();
        engine.write_vt(b"\x1b[2JL0\r\nL1\r\nL2\r\nL3\r\nL4\r\nL5\r\nL6\r\nL7\r\nL8\r\nPROMPT>");
        engine.scroll_viewport_top();
        let sb = engine.snapshot().unwrap().scrollbar();
        assert!(
            sb.offset < sb.total.saturating_sub(sb.len),
            "precondition: viewport scrolled away from the bottom"
        );
    }
    // echo_pending = this read is a ConPTY echo of user input.
    machine.conpty_resize_echo_realign = true;
    machine.conpty_resize_echo_pending = true;

    let mut state = PtyState::default();
    let mut read_buf = [0u8; READ_BUFFER_SIZE];
    machine.pty_read(&mut state, &mut read_buf).unwrap();

    // The echo went to the active screen (off the scrolled-to-top view); scroll
    // to the bottom to observe where it landed.
    let snapshot = {
        let mut engine = machine.ghostty.lock();
        engine.scroll_viewport_bottom();
        engine.snapshot().unwrap()
    };
    // 'Z' lands exactly once, on the prompt row — not a history row.
    let mut z_rows = Vec::new();
    let mut prompt_y = None;
    for y in 0..snapshot.rows() as u16 {
        let text = snapshot_row_text(&snapshot, y);
        if text.contains('Z') {
            z_rows.push(y);
        }
        if text.contains("PROMPT") {
            prompt_y = Some(y);
        }
    }
    assert_eq!(z_rows.len(), 1, "echo 'Z' must appear exactly once");
    assert_eq!(
        Some(z_rows[0]),
        prompt_y,
        "echo 'Z' must land on the prompt row, not a history row"
    );
}

// ---- command-blocks: pty_read emits CommandFinished with the launch cwd ----

#[derive(Clone)]
struct CollectingListener(Arc<Mutex<Vec<event::TerminalEvent>>>);

impl event::EventListener for CollectingListener {
    fn event(&self) -> (Option<event::TerminalEvent>, bool) {
        (None, false)
    }

    fn send_event(&self, event: event::TerminalEvent, _: event::WindowId) {
        self.0.lock().push(event);
    }
}

/// Drive one `pty_read` over `stream` and return the emitted events plus the
/// machine (for engine-state assertions).
fn pty_read_events(
    stream: &[u8],
) -> (
    Vec<event::TerminalEvent>,
    PtyPipe<FakePty, CollectingListener>,
) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let pty = FakePty {
        reader: FakeReader {
            data: stream.to_vec(),
        },
        writer: FakeWriter::default(),
    };
    let mut machine = PtyPipe::new(
        Arc::new(FairMutex::new(RenderBuffer::new(80, 24))),
        Arc::new(AtomicU32::new(0)),
        pty,
        CollectingListener(Arc::clone(&events)),
        event::WindowId::from(0),
        0,
        Colors::default(),
        1000,
        true,
    )
    .unwrap();
    let mut state = PtyState::default();
    let mut buf = [0u8; READ_BUFFER_SIZE];
    machine.pty_read(&mut state, &mut buf).unwrap();

    let collected = events.lock().clone();
    (collected, machine)
}

fn command_finished_events(stream: &[u8]) -> Vec<event::CommandCapture> {
    let (events, _machine) = pty_read_events(stream);
    events
        .iter()
        .filter_map(|e| match e {
            event::TerminalEvent::CommandFinished(c) => Some(c.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn pty_read_emits_osc_52_clipboard_store() {
    use crate::clipboard::ClipboardType;
    use crate::event::TerminalEvent;

    let (events, _) = pty_read_events(b"\x1b]52;c;Y2xhdWRlLWNvcHk=\x07");
    assert!(events.iter().any(|event| {
        matches!(
            event,
            TerminalEvent::ClipboardStore(ClipboardType::Clipboard, text)
                if text == "claude-copy"
        )
    }));
}

/// Full synthetic session: the ps1's `;A;B;C` prime, the first prompt
/// (OSC 7 origin + trust-establishing `;D`), then a `cd dest` whose completing
/// prompt reports OSC 7 dest BEFORE its `;D`. Exactly one block; its cwd is the
/// launch directory (origin), not the destination; no spurious block-0 for the prime.
#[test]
fn pty_read_emits_command_finished_with_launch_cwd() {
    let stream = b"\x1b]133;A\x07\x1b]133;B\x07\x1b]133;C\x07\
\x1b]7;file:///C:/origin\x07\x1b]133;D;0\x07\x1b]133;A\x07PS> \x1b]133;B\x07\
cd dest\r\n\x1b]133;C\x07\
\x1b]7;file:///C:/dest\x07\x1b]133;D;0\x07\x1b]133;A\x07PS dest> \x1b]133;B\x07";
    let blocks = command_finished_events(stream);
    assert_eq!(
        blocks.len(),
        1,
        "one block for the cd; none for the synthetic prime"
    );
    let b = &blocks[0];
    assert_eq!(b.command, "cd dest");
    assert_eq!(b.exit_code, Some(0));
    assert!(b.started_at <= b.ended_at);
    let cwd = b
        .cwd
        .as_ref()
        .expect("launch cwd latched from OSC 7")
        .to_string_lossy()
        .to_string();
    assert!(
        cwd.contains("origin") && !cwd.contains("dest"),
        "cwd must be the launch (origin) directory, got {cwd}"
    );
}

#[test]
fn pty_read_boundary_protocol_snapshots_clears_then_starts_next_prompt() {
    use crate::event::{BlockEvent, TerminalEvent};

    let stream = b"\x1b]133;A\x07\x1b]133;B\x07\x1b]133;C\x07\
\x1b]133;D;0\x07\x1b]133;A\x07PS> \x1b]133;B\x07\
echo hi\r\n\x1b]133;C\x07hi\r\n\
\x1b]133;D;0\x07\x1b[2J\x1b[3J\x1b[H\x1b]133;A\x07PS> \x1b]133;B\x07";
    let (events, machine) = pty_read_events(stream);

    let mut shape = Vec::new();
    let mut handles = Vec::new();
    for event in &events {
        if let TerminalEvent::BlockBatch(batch) = event {
            for event in batch {
                match event {
                    BlockEvent::HistoryCleared => shape.push("cleared".into()),
                    BlockEvent::EngineBlock { seq, rows, handle } => {
                        handles.push(*handle);
                        shape.push(format!("block{seq}:{rows}"))
                    }
                    BlockEvent::EngineBlocksSync(live) => {
                        shape.push(format!("sync:{}", live.len()))
                    }
                }
            }
        }
    }

    assert_eq!(
        shape,
        vec!["block1:2", "sync:1"],
        "the command freezes into one engine block before the protocol clear"
    );

    // The frozen block holds the command's rows, readable via BlockRef.
    {
        let engine = machine.ghostty.lock();
        let block = engine.block_acquire(handles[0]).expect("block alive");
        let text = block.format_range((0, 0), (1, 79), true, true).unwrap();
        assert_eq!(text, "PS> echo hi\nhi");
    }

    let snapshot = machine.ghostty.lock().snapshot().unwrap();
    assert_eq!(snapshot_row_text(&snapshot, 0), "PS>");
    assert!(
        (1..snapshot.rows() as u16).all(|y| snapshot_row_text(&snapshot, y).is_empty()),
        "old command output must not remain in the new block"
    );
}

/// A user clear announced by the Clear-Host wrapper (`;K`): the frozen
/// history drops (HistoryCleared), the engine is wiped, and the session
/// keeps working — the next command harvests normally from row 0.
#[test]
fn pty_read_history_clear_mark_drops_history_and_wipes_engine() {
    use crate::event::{BlockEvent, TerminalEvent};

    let stream = b"\x1b]133;A\x07\x1b]133;B\x07\x1b]133;C\x07\
\x1b]133;D;0\x07\x1b]133;A\x07PS> \x1b]133;B\x07\
echo hi\r\n\x1b]133;C\x07hi\r\n\
\x1b]133;D;0\x07\x1b[2J\x1b[3J\x1b[H\x1b]133;A\x07PS> \x1b]133;B\x07\
clear\r\n\x1b]133;C\x07\x1b]133;K\x07\x1b[2J\x1b[3J\x1b[H";
    let (events, machine) = pty_read_events(stream);

    let cleared_at = events.iter().position(|event| {
        matches!(event, TerminalEvent::BlockBatch(batch)
            if batch.contains(&BlockEvent::HistoryCleared))
    });
    assert!(
        cleared_at.is_some(),
        "the ;K mark must surface HistoryCleared: {events:?}"
    );

    let snapshot = machine.ghostty.lock().snapshot().unwrap();
    assert!(
        (0..snapshot.rows() as u16).all(|y| snapshot_row_text(&snapshot, y).is_empty()),
        "the engine must be wiped at the ;K mark"
    );
}

#[test]
fn pty_read_boundary_protocol_does_not_clear_alt_screen() {
    let stream = b"\x1b]133;A\x07\x1b]133;B\x07\x1b]133;C\x07\
\x1b]133;D;0\x07\x1b]133;A\x07PS> \x1b]133;B\x07\
vim\r\n\x1b]133;C\x07\x1b[?1049hTUI\x1b]133;D;0\x07";
    let (_events, machine) = pty_read_events(stream);

    let mut engine = machine.ghostty.lock();
    assert!(engine.mode(ghostty::mode::ALT_SCREEN));
    let snapshot = engine.snapshot().unwrap();
    let rows: Vec<_> = (0..snapshot.rows() as u16)
        .map(|y| snapshot_row_text(&snapshot, y))
        .collect();
    assert!(
        rows.iter().any(|row| row.contains("TUI")),
        "trusted ;D inside alt screen must not write the block-boundary clear"
    );
}

#[test]
fn pty_read_emits_no_command_for_untrusted_stream() {
    // Out-of-order lifecycle (starts at ;B): untrusted, nothing recorded.
    let blocks = command_finished_events(b"\x1b]133;B\x07evil\x1b]133;C\x07out\x1b]133;D;0\x07");
    assert!(blocks.is_empty());
}

/// A two-command session produces ordered CommandStarted/CommandFinished pairs with
/// matching split segment sequence numbers.
#[test]
fn pty_read_emits_start_and_finish_events_with_seq() {
    use crate::event::TerminalEvent;
    let stream = b"\x1b]133;A\x07\x1b]133;B\x07\x1b]133;C\x07\
\x1b]7;file:///C:/w\x07\x1b]133;D;0\x07\x1b]133;A\x07PS> \x1b]133;B\x07\
echo one\r\n\x1b]133;C\x07one\r\n\
\x1b]7;file:///C:/w\x07\x1b]133;D;0\x07\x1b]133;A\x07PS> \x1b]133;B\x07\
echo two\r\n\x1b]133;C\x07two\r\n\
\x1b]7;file:///C:/w\x07\x1b]133;D;0\x07\x1b]133;A\x07PS> \x1b]133;B\x07";
    let (events, machine) = pty_read_events(stream);

    let starts: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            TerminalEvent::CommandStarted(s) => Some(s.clone()),
            _ => None,
        })
        .collect();
    let finishes: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            TerminalEvent::CommandFinished(c) => Some(c.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        starts.len(),
        2,
        "one start per real command, none for the prime"
    );
    assert_eq!(finishes.len(), 2);
    assert_eq!(starts[0].command, "echo one");
    assert_eq!(starts[1].command, "echo two");
    assert_eq!(finishes[0].command, "echo one");
    assert_eq!(finishes[1].command, "echo two");
    assert_eq!(starts[0].seq, finishes[0].seq);
    assert_eq!(starts[1].seq, finishes[1].seq);
    assert!(starts[0].seq < starts[1].seq);

    // Mark forwarding is working when the engine tags the
    // prompt rows — the drift-correction ground truth for the view.
    assert!(
        machine.ghostty.lock().has_prompt_tagged_row(),
        "engine rows must carry semantic prompt tags after mark forwarding"
    );
}
