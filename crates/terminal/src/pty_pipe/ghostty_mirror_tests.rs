use std::sync::Arc;
use std::sync::atomic::AtomicU32;
use std::{io, sync, time};

use nmt_config::colors::Colors;
use nmt_platform::{EventedPty, ProcessReadWrite, WinsizeBuilder};
use parking_lot::Mutex;

use crate::event::{self, VoidListener};
use crate::pty_pipe::powershell_compatibility::RESIZE_INPUT_DELAY;
use crate::pty_pipe::{
    Interest, Poll, PtyPipe, PtyState, READ_BUFFER_SIZE, SNAPSHOT_MIN_INTERVAL,
    SYNC_OUTPUT_TIMEOUT, SessionOptions, Token, Waker, mode, publish_render_buffer, start_session,
};
use crate::publication::FrameStore;
use crate::render_buffer::RenderBuffer;
use crate::{ansi, ghostty};

#[test]
fn failed_capture_does_not_publish_back_buffer() {
    let front = FrameStore::new(RenderBuffer::new(2, 1));
    let mut back = RenderBuffer::new(3, 1);
    let mut failed = false;

    assert!(!publish_render_buffer(
        &front,
        &mut back,
        Err(ghostty::Error::InvalidValue),
        false,
        &mut failed,
    ));
    assert!(failed);
    assert_eq!(front.load().cols(), 2);
    assert_eq!(back.cols(), 3);

    assert!(publish_render_buffer(
        &front,
        &mut back,
        Ok(()),
        false,
        &mut failed
    ));
    assert!(!failed);
    assert_eq!(front.load().cols(), 3);
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

fn resized_pipe(initial: &[u8]) -> PtyPipe<FakePty, VoidListener> {
    let mut machine = PtyPipe::new(
        Arc::new(FrameStore::new(RenderBuffer::new(80, 24))),
        Arc::new(AtomicU32::new(0)),
        FakePty {
            reader: FakeReader { data: Vec::new() },
            writer: FakeWriter::default(),
        },
        VoidListener {},
        &SessionOptions {
            cols: 80,
            rows: 24,
            route_id: 0,
            colors: Colors::default(),
            cursor_shape: ansi::CursorShape::Block,
            scrollback_lines: 1000,
            engine_blocks: false,
            terminal_responses: true,
            output_sink: None,
        },
    )
    .unwrap();

    machine.ghostty.write_vt(initial);

    machine.on_resize(WinsizeBuilder {
        cols: 80,
        rows: 25,
        width: 640,
        height: 400,
    });

    machine
}

#[test]
fn resize_reads_preserve_repaint_across_every_split() {
    let repaint = b"\x1b[H\x1b[2JHEADER\x1b[2;1H>";
    let mut baseline = resized_pipe(b"\x1b[18;1H>");

    baseline.ghostty.write_vt(repaint);

    let expected = baseline.ghostty.format_text(None, false, true).unwrap();

    for split in 1..repaint.len() {
        let mut machine = resized_pipe(b"\x1b[18;1H>");

        machine.on_pty_chunk(&repaint[..split]);
        machine.on_pty_chunk(&repaint[split..]);

        let snapshot = machine.ghostty.snapshot().unwrap();

        assert_eq!(snapshot_row_text(&snapshot, 0), "HEADER", "split={split}");
        assert_eq!(snapshot_row_text(&snapshot, 1), ">", "split={split}");
        assert_eq!(machine.ghostty.active_cursor_row(), Some(1));
        assert_eq!(
            machine.ghostty.format_text(None, false, true).unwrap(),
            expected,
            "split={split} changed screen or history"
        );
    }
}

#[test]
fn resize_reads_preserve_bottom_text_during_partial_erase() {
    let mut machine = resized_pipe(b"\x1b[24;1HBOTTOM\x1b[2;1H>");
    let input = b"\x1b[1J\x1b[5;1Hnew";
    let forwarded = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&forwarded);

    machine.set_output_sink(move |bytes| sink.lock().extend_from_slice(&bytes));

    machine.on_pty_chunk(input);

    let snapshot = machine.ghostty.snapshot().unwrap();

    assert_eq!(snapshot_row_text(&snapshot, 23), "BOTTOM");
    assert_eq!(snapshot_row_text(&snapshot, 4), "new");
    assert_eq!(&*forwarded.lock(), input);
}

#[test]
fn resize_reads_leave_completed_repaint_and_later_echo_in_place() {
    let mut machine = resized_pipe(b"\x1b[1;1HHISTORY\x1b[18;1H>");

    machine.on_pty_chunk(b"\x1b[18;1H\x1b[J>");
    machine.on_pty_chunk(b"\x1b[19;1Hresult");
    machine.on_pty_chunk(b"\r\x1b[6CXYZ");

    let snapshot = machine.ghostty.snapshot().unwrap();

    assert_eq!(snapshot_row_text(&snapshot, 0), "HISTORY");
    assert_eq!(snapshot_row_text(&snapshot, 17), ">");
    assert_eq!(snapshot_row_text(&snapshot, 18), "resultXYZ");
    assert_eq!(machine.ghostty.active_cursor_row(), Some(18));
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
    budget: Option<usize>,
    flush_pending: bool,
    operations: Vec<String>,
    observer: Option<sync::mpsc::Sender<Vec<u8>>>,
}

impl io::Write for FakeWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let buf = if let Some(budget) = &mut self.budget {
            if *budget == 0 {
                return Err(io::ErrorKind::WouldBlock.into());
            }

            let count = buf.len().min(*budget);

            *budget -= count;

            &buf[..count]
        } else {
            buf
        };

        self.data.extend_from_slice(buf);

        self.operations
            .push(format!("input:{}", String::from_utf8_lossy(buf)));

        if let Some(observer) = &self.observer {
            observer.send(buf.to_vec()).unwrap();
        }

        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.flush_pending {
            Err(io::ErrorKind::WouldBlock.into())
        } else {
            Ok(())
        }
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

    fn set_winsize(&mut self, size: WinsizeBuilder) -> io::Result<()> {
        self.writer
            .operations
            .push(format!("resize:{}x{}", size.cols, size.rows));

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
        vec![self.write_token()]
    }
}

impl EventedPty for FakePty {
    fn child_event_token(&self) -> Token {
        Token(3)
    }

    fn child_exited(&mut self) -> bool {
        false
    }
}

#[test]
fn queued_resize_cannot_overtake_partial_or_buffered_input() {
    let mut machine = resized_pipe(b"");
    let mut state = PtyState::default();

    machine.pty.writer.operations.clear();
    machine.pty.writer.budget = Some(2);
    machine.pty.writer.flush_pending = true;

    let sender = machine.channel();

    sender
        .send(event::Msg::Input(b"abc".to_vec().into()))
        .unwrap();

    sender
        .send(event::Msg::Resize(WinsizeBuilder {
            cols: 60,
            rows: 20,
            width: 480,
            height: 360,
        }))
        .unwrap();

    sender
        .send(event::Msg::Input(b"Z".to_vec().into()))
        .unwrap();

    assert!(machine.drain_recv_channel(&mut state));
    assert_eq!(machine.ghostty.cols(), 80);

    machine.pty_write(&mut state).unwrap();

    assert!(machine.drain_recv_channel(&mut state));
    assert_eq!(machine.ghostty.cols(), 80);
    assert_eq!(machine.pty.writer.data, b"ab");

    machine.pty.writer.budget = None;
    machine.pty_write(&mut state).unwrap();

    assert!(machine.drain_recv_channel(&mut state));
    assert_eq!(machine.ghostty.cols(), 80);
    assert_eq!(machine.pty.writer.data, b"abc");

    machine.pty.writer.flush_pending = false;

    assert!(machine.drain_recv_channel(&mut state));

    machine.pty_write(&mut state).unwrap();

    assert_eq!(
        machine.pty.writer.operations,
        ["input:ab", "input:c", "resize:60x20", "input:Z"]
    );
}

#[test]
fn queued_resizes_coalesce_only_until_the_next_input() {
    let mut machine = resized_pipe(b"");
    let mut state = PtyState::default();

    machine.pty.writer.operations.clear();

    let sender = machine.channel();

    for cols in [50, 60] {
        sender
            .send(event::Msg::Resize(WinsizeBuilder {
                cols,
                rows: 20,
                width: cols * 8,
                height: 360,
            }))
            .unwrap();
    }

    sender
        .send(event::Msg::Input(b"A".to_vec().into()))
        .unwrap();

    sender
        .send(event::Msg::Resize(WinsizeBuilder {
            cols: 100,
            rows: 30,
            width: 800,
            height: 540,
        }))
        .unwrap();

    sender
        .send(event::Msg::Input(b"B".to_vec().into()))
        .unwrap();

    assert!(machine.drain_recv_channel(&mut state));

    machine.pty_write(&mut state).unwrap();

    assert!(machine.drain_recv_channel(&mut state));

    machine.pty_write(&mut state).unwrap();

    assert_eq!(
        machine.pty.writer.operations,
        ["resize:60x20", "input:A", "resize:100x30", "input:B"]
    );
}

#[test]
fn input_released_after_resize_wakes_an_otherwise_idle_loop() {
    use std::thread;

    let mut machine = resized_pipe(b"");
    let (written, received) = sync::mpsc::channel();

    machine.pty.writer.observer = Some(written);

    let sender = machine.channel();

    sender
        .send(event::Msg::Input(b"A".to_vec().into()))
        .unwrap();

    sender
        .send(event::Msg::Resize(WinsizeBuilder {
            cols: 60,
            rows: 20,
            width: 480,
            height: 360,
        }))
        .unwrap();

    sender
        .send(event::Msg::Input(b"B".to_vec().into()))
        .unwrap();

    let worker = thread::Builder::new()
        .stack_size(4 * 1024 * 1024)
        .spawn(move || machine.run_event_loop())
        .unwrap();

    let first = received.recv_timeout(time::Duration::from_secs(2));
    let second = received.recv_timeout(time::Duration::from_secs(2));

    sender.send(event::Msg::Shutdown).unwrap();
    worker.join().unwrap();

    assert_eq!(first.unwrap(), b"A");
    assert_eq!(
        second.expect("input after resize waited for unrelated activity"),
        b"B"
    );
}

#[test]
fn pending_resize_allows_cursor_replies_and_immediate_shutdown() {
    let mut machine = resized_pipe(b"\x1b[4;5H");
    let mut state = PtyState::default();

    machine.pty.writer.flush_pending = true;

    let sender = machine.channel();

    sender
        .send(event::Msg::Resize(WinsizeBuilder {
            cols: 60,
            rows: 20,
            width: 480,
            height: 360,
        }))
        .unwrap();

    sender
        .send(event::Msg::Input(b"later".to_vec().into()))
        .unwrap();

    assert!(machine.drain_recv_channel(&mut state));

    machine.pty.reader.data.extend_from_slice(b"\x1b[6n");

    machine
        .pty_read(&mut state, &mut [0; READ_BUFFER_SIZE])
        .unwrap();

    machine.pty_write(&mut state).unwrap();

    assert_eq!(machine.pty.writer.data, b"\x1b[4;5R");
    assert_eq!(machine.ghostty.cols(), 80);

    sender.send(event::Msg::Shutdown).unwrap();

    assert!(!machine.drain_recv_channel(&mut state));
    assert_eq!(machine.ghostty.cols(), 80);
    assert_eq!(machine.pty.writer.data, b"\x1b[4;5R");
}

#[test]
fn powershell_pause_keeps_replies_live_and_disabling_releases_ordered_input() {
    let mut machine = resized_pipe(b"\x1b[4;5H");
    let mut state = PtyState::default();
    let sender = machine.channel();

    sender
        .send(event::Msg::PowerShellCompatibility(true))
        .unwrap();

    sender
        .send(event::Msg::Resize(WinsizeBuilder {
            cols: 60,
            rows: 20,
            width: 480,
            height: 360,
        }))
        .unwrap();

    sender
        .send(event::Msg::Input(b"A".to_vec().into()))
        .unwrap();

    sender
        .send(event::Msg::Resize(WinsizeBuilder {
            cols: 100,
            rows: 30,
            width: 800,
            height: 540,
        }))
        .unwrap();

    sender
        .send(event::Msg::Input(b"B".to_vec().into()))
        .unwrap();

    assert!(machine.drain_recv_channel(&mut state));
    assert!(!state.needs_write());
    assert_eq!(machine.ghostty.cols(), 60);

    machine.pty.reader.data.extend_from_slice(b"\x1b[6n");

    machine
        .pty_read(&mut state, &mut [0; READ_BUFFER_SIZE])
        .unwrap();

    machine.pty_write(&mut state).unwrap();

    assert_eq!(machine.pty.writer.data, b"\x1b[4;5R");

    sender
        .send(event::Msg::PowerShellCompatibility(false))
        .unwrap();

    assert!(machine.drain_recv_channel(&mut state));

    machine.pty_write(&mut state).unwrap();

    assert_eq!(machine.ghostty.cols(), 60);
    assert!(machine.drain_recv_channel(&mut state));

    machine.pty_write(&mut state).unwrap();

    assert_eq!(machine.ghostty.cols(), 100);
    assert_eq!(machine.pty.writer.data, b"\x1b[4;5RAB");

    sender
        .send(event::Msg::PowerShellCompatibility(true))
        .unwrap();

    sender
        .send(event::Msg::Resize(WinsizeBuilder {
            cols: 60,
            rows: 20,
            width: 480,
            height: 360,
        }))
        .unwrap();

    sender
        .send(event::Msg::Input(b"discard-on-close".to_vec().into()))
        .unwrap();

    assert!(machine.drain_recv_channel(&mut state));

    sender.send(event::Msg::Shutdown).unwrap();

    assert!(!machine.drain_recv_channel(&mut state));
    assert_eq!(machine.pty.writer.data, b"\x1b[4;5RAB");
}

#[test]
fn powershell_input_deadline_wakes_a_quiet_event_loop() {
    use std::thread;

    let mut machine = resized_pipe(b"");
    let (written, received) = sync::mpsc::channel();

    machine.pty.writer.observer = Some(written);

    let sender = machine.channel();

    sender
        .send(event::Msg::PowerShellCompatibility(true))
        .unwrap();

    sender
        .send(event::Msg::Resize(WinsizeBuilder {
            cols: 60,
            rows: 20,
            width: 480,
            height: 360,
        }))
        .unwrap();

    sender
        .send(event::Msg::Input(b"later".to_vec().into()))
        .unwrap();

    let started = time::Instant::now();

    let worker = thread::Builder::new()
        .stack_size(4 * 1024 * 1024)
        .spawn(move || machine.run_event_loop())
        .unwrap();

    let bytes = received.recv_timeout(time::Duration::from_secs(2));
    let elapsed = started.elapsed();

    sender.send(event::Msg::Shutdown).unwrap();
    worker.join().unwrap();

    assert_eq!(bytes.expect("input deadline did not wake poll"), b"later");
    assert!(elapsed >= RESIZE_INPUT_DELAY);
}

#[test]
fn powershell_pause_skips_alternate_screen_and_unchanged_grid_sizes() {
    let mut machine = resized_pipe(b"");
    let mut state = PtyState::default();
    let sender = machine.channel();

    sender
        .send(event::Msg::PowerShellCompatibility(true))
        .unwrap();

    assert!(machine.drain_recv_channel(&mut state));

    machine.on_resize(WinsizeBuilder {
        cols: 80,
        rows: 25,
        width: 640,
        height: 400,
    });

    sender
        .send(event::Msg::Input(b"same-size".to_vec().into()))
        .unwrap();

    assert!(machine.drain_recv_channel(&mut state));

    machine.pty_write(&mut state).unwrap();

    machine.on_resize(WinsizeBuilder {
        cols: 60,
        rows: 20,
        width: 480,
        height: 360,
    });

    machine.on_pty_chunk(b"\x1b[?1049h");

    sender
        .send(event::Msg::Input(b"alternate".to_vec().into()))
        .unwrap();

    assert!(machine.drain_recv_channel(&mut state));

    machine.pty_write(&mut state).unwrap();

    machine.on_resize(WinsizeBuilder {
        cols: 80,
        rows: 25,
        width: 640,
        height: 400,
    });

    machine.on_pty_chunk(b"\x1b[?1049l");

    sender
        .send(event::Msg::Input(b"returned".to_vec().into()))
        .unwrap();

    assert!(machine.drain_recv_channel(&mut state));

    machine.pty_write(&mut state).unwrap();

    assert_eq!(machine.pty.writer.data, b"same-sizealternatereturned");
}

#[test]
fn terminal_replies_resume_after_partial_writes_in_input_order() {
    let pty = FakePty {
        reader: FakeReader {
            data: b"\x1b[6n".to_vec(),
        },
        writer: FakeWriter {
            budget: Some(2),
            ..FakeWriter::default()
        },
    };

    let mut machine = PtyPipe::new(
        Arc::new(FrameStore::new(RenderBuffer::new(20, 3))),
        Arc::new(AtomicU32::new(0)),
        pty,
        VoidListener {},
        &SessionOptions {
            cols: 20,
            rows: 3,
            route_id: 0,
            colors: Colors::default(),
            cursor_shape: ansi::CursorShape::Block,
            scrollback_lines: 1000,
            engine_blocks: false,
            terminal_responses: true,
            output_sink: None,
        },
    )
    .unwrap();

    let mut state = PtyState::default();

    state.write_list.push_back(b"in".to_vec().into());

    machine
        .pty_read(&mut state, &mut [0; READ_BUFFER_SIZE])
        .unwrap();

    assert!(machine.pty.writer.data.is_empty());

    machine.pty_write(&mut state).unwrap();

    assert_eq!(machine.pty.writer.data, b"in");
    assert!(state.needs_write());

    machine.pty.writer.budget = Some(3);
    machine.pty_write(&mut state).unwrap();

    assert_eq!(machine.pty.writer.data, b"in\x1b[1");
    assert!(state.needs_write());

    machine.pty.writer.budget = None;
    machine.pty_write(&mut state).unwrap();

    assert_eq!(machine.pty.writer.data, b"in\x1b[1;1R");
    assert!(!state.needs_write());
}

#[test]
fn dropping_session_handles_releases_worker_resources_before_returning() {
    let lifetime = Arc::new(());
    let released = Arc::downgrade(&lifetime);

    let handles = start_session(
        FakePty {
            reader: FakeReader { data: Vec::new() },
            writer: FakeWriter::default(),
        },
        VoidListener {},
        SessionOptions {
            cols: 20,
            rows: 3,
            route_id: 0,
            colors: Colors::default(),
            cursor_shape: ansi::CursorShape::Block,
            scrollback_lines: 1000,
            engine_blocks: false,
            terminal_responses: true,
            output_sink: Some(Arc::new(move |_| {
                let _ = &lifetime;
            })),
        },
    )
    .unwrap();

    drop(handles);

    assert!(
        released.upgrade().is_none(),
        "worker resources must be released before close returns"
    );
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
        Arc::new(FrameStore::new(RenderBuffer::new(20, 3))),
        Arc::new(AtomicU32::new(0)),
        pty,
        VoidListener {},
        &SessionOptions {
            cols: 20,
            rows: 3,
            route_id: 0,
            colors: Colors::default(),
            cursor_shape: ansi::CursorShape::Block,
            scrollback_lines: 1000,
            engine_blocks: false,
            terminal_responses: true,
            output_sink: None,
        },
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
    let render_buffer = Arc::new(FrameStore::new(RenderBuffer::new(20, 3)));
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
        &SessionOptions {
            cols: 20,
            rows: 3,
            route_id: 0,
            colors: Colors::default(),
            cursor_shape: ansi::CursorShape::Block,
            scrollback_lines: 1000,
            engine_blocks: false,
            terminal_responses: true,
            output_sink: None,
        },
    )
    .unwrap();

    {
        let engine = &mut machine.ghostty;

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

    let buffer = render_buffer.load();

    assert_eq!(buffer.rows(), 5);
    assert_eq!(render_buffer_row_text(&buffer, 0), "resize-ok");
}

#[test]
fn synchronized_output_keeps_published_cursor_on_previous_frame_until_commit() {
    let render_buffer = Arc::new(FrameStore::new(RenderBuffer::new(20, 3)));

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
        &SessionOptions {
            cols: 20,
            rows: 3,
            route_id: 0,
            colors: Colors::default(),
            cursor_shape: ansi::CursorShape::Block,
            scrollback_lines: 1000,
            engine_blocks: false,
            terminal_responses: true,
            output_sink: None,
        },
    )
    .unwrap();

    let mut state = PtyState::default();
    let mut buf = [0u8; READ_BUFFER_SIZE];

    machine.pty_read(&mut state, &mut buf).unwrap();

    assert_eq!(render_buffer.load().cursor().row.0, 2);

    machine
        .pty
        .reader
        .data
        .extend_from_slice(b"\x1b[?2026h\x1b[1;1HWorking");

    machine.pty_read(&mut state, &mut buf).unwrap();

    {
        let buffer = render_buffer.load();

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
        let buffer = render_buffer.load();

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
        let buffer = render_buffer.load();

        assert_eq!(buffer.cursor().row.0, 1);
        assert_eq!(render_buffer_row_text(&buffer, 1), "Stuck");
    }

    assert!(!machine.ghostty.mode(mode::SYNC_OUTPUT));
}

#[test]
fn osc_progress_hides_published_cursor_until_removed() {
    let render_buffer = Arc::new(FrameStore::new(RenderBuffer::new(80, 3)));

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
        &SessionOptions {
            cols: 80,
            rows: 3,
            route_id: 0,
            colors: Colors::default(),
            cursor_shape: ansi::CursorShape::Block,
            scrollback_lines: 1000,
            engine_blocks: false,
            terminal_responses: true,
            output_sink: None,
        },
    )
    .unwrap();

    machine
        .ghostty
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

    machine.last_snapshot_at = Some(time::Instant::now() - SNAPSHOT_MIN_INTERVAL);
    machine.pty_read(&mut state, &mut buf).unwrap();

    {
        let buffer = render_buffer.load();

        assert_eq!(buffer.cursor_shape(), ansi::CursorShape::Beam);
        assert!(!buffer.cursor_visible(), "active progress hides the cursor");
    }

    machine
        .pty
        .reader
        .data
        .extend_from_slice(b"\x1b]9;4;0;\x1b\\");

    machine.last_snapshot_at = Some(time::Instant::now() - SNAPSHOT_MIN_INTERVAL);
    machine.pty_read(&mut state, &mut buf).unwrap();

    assert!(
        render_buffer.load().cursor_visible(),
        "removing progress restores the terminal cursor state"
    );
}

#[test]
fn conpty_echo_after_resize_preserves_addressed_row() {
    let render_buffer = Arc::new(FrameStore::new(RenderBuffer::new(134, 42)));
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
        &SessionOptions {
            cols: 134,
            rows: 42,
            route_id: 0,
            colors: Colors::default(),
            cursor_shape: ansi::CursorShape::Block,
            scrollback_lines: 1000,
            engine_blocks: false,
            terminal_responses: true,
            output_sink: None,
        },
    )
    .unwrap();

    {
        let engine = &mut machine.ghostty;

        engine.write_vt(b"\x1b[2J\x1b[10;1HHISTORY\x1b[42;1HC:\\Workspace\\NiumaTerm>");
    }

    machine.on_resize(WinsizeBuilder {
        cols: machine.ghostty.cols(),
        rows: machine.ghostty.rows(),
        width: machine.ghostty.cols() * 8,
        height: machine.ghostty.rows() * 16,
    });

    let mut state = PtyState::default();
    let mut read_buf = [0u8; READ_BUFFER_SIZE];

    machine.pty_read(&mut state, &mut read_buf).unwrap();

    let snapshot = machine.ghostty.snapshot().unwrap();

    assert_eq!(
        snapshot_row_text(&snapshot, 9),
        format!("HISTORY{}x", " ".repeat(16))
    );
    assert_eq!(
        snapshot_row_text(&snapshot, 41),
        "C:\\Workspace\\NiumaTerm>"
    );
    assert_eq!(machine.ghostty.active_cursor_row(), Some(9));
}

#[test]
fn conpty_repaint_after_resize_uses_active_screen_when_scrolled() {
    let render_buffer = Arc::new(FrameStore::new(RenderBuffer::new(20, 4)));
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
        &SessionOptions {
            cols: 20,
            rows: 4,
            route_id: 0,
            colors: Colors::default(),
            cursor_shape: ansi::CursorShape::Block,
            scrollback_lines: 1000,
            engine_blocks: false,
            terminal_responses: true,
            output_sink: None,
        },
    )
    .unwrap();

    {
        let engine = &mut machine.ghostty;

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

    machine.on_resize(WinsizeBuilder {
        cols: machine.ghostty.cols(),
        rows: machine.ghostty.rows(),
        width: machine.ghostty.cols() * 8,
        height: machine.ghostty.rows() * 16,
    });

    let mut state = PtyState::default();
    let mut read_buf = [0u8; READ_BUFFER_SIZE];

    machine.pty_read(&mut state, &mut read_buf).unwrap();

    // CUP addresses the active screen even while the viewport shows history.
    let (active_row, snapshot) = {
        let engine = &mut machine.ghostty;
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

// ---- command-blocks: pty_read emits CommandFinished with the launch cwd ----

#[derive(Clone)]
struct CollectingListener(Arc<Mutex<Vec<event::TerminalEvent>>>);

impl event::EventListener for CollectingListener {
    fn send_event(&self, event: event::TerminalEvent) {
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
        Arc::new(FrameStore::new(RenderBuffer::new(80, 24))),
        Arc::new(AtomicU32::new(0)),
        pty,
        CollectingListener(Arc::clone(&events)),
        &SessionOptions {
            cols: 80,
            rows: 24,
            route_id: 0,
            colors: Colors::default(),
            cursor_shape: ansi::CursorShape::Block,
            scrollback_lines: 1000,
            engine_blocks: true,
            terminal_responses: true,
            output_sink: None,
        },
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

    let (events, mut machine) = pty_read_events(stream);

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
        let engine = &mut machine.ghostty;
        let block = engine.block_acquire(handles[0]).expect("block alive");
        let text = block.format_range((0, 0), (1, 79), true, true).unwrap();

        assert_eq!(text, "PS> echo hi\nhi");
    }

    let snapshot = machine.ghostty.snapshot().unwrap();

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

    let (events, mut machine) = pty_read_events(stream);

    let cleared_at = events.iter().position(|event| {
        matches!(event, TerminalEvent::BlockBatch(batch)
            if batch.contains(&BlockEvent::HistoryCleared))
    });

    assert!(
        cleared_at.is_some(),
        "the ;K mark must surface HistoryCleared: {events:?}"
    );

    let snapshot = machine.ghostty.snapshot().unwrap();

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

    let (_events, mut machine) = pty_read_events(stream);

    let engine = &mut machine.ghostty;

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

    let (events, mut machine) = pty_read_events(stream);

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
        machine.ghostty.has_prompt_tagged_row(),
        "engine rows must carry semantic prompt tags after mark forwarding"
    );
}
