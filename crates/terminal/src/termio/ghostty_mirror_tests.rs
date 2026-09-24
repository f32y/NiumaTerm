use std::future::{Future, poll_fn};
use std::io::{Read, Write};
use std::pin::{Pin, pin};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::task::{Context, Poll as TaskPoll, Waker};
use std::{io, mem, sync, time};

use futures::executor::block_on;
use nmt_config::CursorShape;
use nmt_config::colors::{Colors, NamedColor};
use nmt_platform::{AsyncPty, WinsizeBuilder, poll_nonblocking};
use parking_lot::Mutex;
use tokio::runtime::{Builder, Runtime};
use tokio::sync::oneshot;
use tokio::time::{Instant, sleep, timeout};

use crate::event::{self, VoidListener};
use crate::ghostty;
use crate::render_buffer::{FrameStore, RenderBuffer};
use crate::termio::powershell_compatibility::RESIZE_INPUT_DELAY;
use crate::termio::{
    PtyState, READ_BUFFER_SIZE, SNAPSHOT_MIN_INTERVAL, SYNC_OUTPUT_TIMEOUT, SessionHandles,
    SessionOptions, Step, Termio, mode, publish_render_buffer, start_session,
};

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

fn noop_cx() -> Context<'static> {
    Context::from_waker(Waker::noop())
}

fn drain<U: event::EventListener + Send + 'static>(
    machine: &mut Termio<FakePty, U>,
    state: &mut PtyState,
) -> Step {
    machine.drain_recv_channel(state, &mut noop_cx())
}

/// A current-thread runtime on paused time: sleeps and the PTY task's own
/// deadlines advance together, so wake counts are exact.
fn paused_runtime() -> Runtime {
    Builder::new_current_thread()
        .enable_all()
        .start_paused(true)
        .build()
        .unwrap()
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

fn resized_pipe(initial: &[u8]) -> Termio<FakePty, VoidListener> {
    let mut machine = Termio::new(
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
            cursor_shape: CursorShape::Block,
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
    resize_completion: Option<oneshot::Receiver<()>>,
    exited: bool,
    write_zero: bool,
    operations: Vec<String>,
    observer: Option<sync::mpsc::Sender<Vec<u8>>>,
}

impl io::Write for FakeWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.write_zero {
            return Ok(0);
        }

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

impl AsyncPty for FakePty {
    fn poll_read(&mut self, cx: &mut Context<'_>, buf: &mut [u8]) -> TaskPoll<io::Result<usize>> {
        match self.reader.read(buf) {
            Ok(0) => TaskPoll::Pending,
            result => poll_nonblocking(cx, result),
        }
    }

    fn poll_write(&mut self, cx: &mut Context<'_>, buf: &[u8]) -> TaskPoll<io::Result<usize>> {
        poll_nonblocking(cx, self.writer.write(buf))
    }

    fn poll_flush(&mut self, cx: &mut Context<'_>) -> TaskPoll<io::Result<()>> {
        poll_nonblocking(cx, self.writer.flush())
    }

    fn poll_exit(&mut self, _: &mut Context<'_>) -> TaskPoll<()> {
        if mem::take(&mut self.writer.exited) {
            TaskPoll::Ready(())
        } else {
            TaskPoll::Pending
        }
    }

    fn poll_resize(
        &mut self,
        cx: &mut Context<'_>,
        size: WinsizeBuilder,
    ) -> TaskPoll<io::Result<()>> {
        if let Some(completion) = &mut self.writer.resize_completion {
            if Pin::new(completion).poll(cx).is_pending() {
                return TaskPoll::Pending;
            }

            self.writer.resize_completion = None;
        }

        self.writer
            .operations
            .push(format!("resize:{}x{}", size.cols, size.rows));

        TaskPoll::Ready(Ok(()))
    }
}

#[test]
fn empty_input_does_not_stall_following_writes() {
    let mut machine = resized_pipe(b"");
    let mut state = PtyState::default();
    let mut cx = noop_cx();

    state.write_list.push_back(Vec::new().into());
    state.write_list.push_back(b"after empty".to_vec().into());

    machine.pty_write(&mut state, &mut cx).unwrap();

    assert_eq!(machine.pty.writer.data, b"after empty");
    assert!(!state.needs_write());

    machine.pty.writer.write_zero = true;

    state.write_list.push_back(b"stalled".to_vec().into());

    assert_eq!(
        machine.pty_write(&mut state, &mut cx).unwrap_err().kind(),
        io::ErrorKind::WriteZero
    );
}

#[test]
fn pending_native_resize_keeps_output_live_and_preserves_input_order() {
    let mut machine = resized_pipe(b"");
    let mut state = PtyState::default();
    let mut cx = noop_cx();
    let mut buf = vec![0; READ_BUFFER_SIZE];
    let (finish_resize, resized) = oneshot::channel();

    machine.pty.writer.resize_completion = Some(resized);
    machine.pty.reader.data = b"output during resize".to_vec();

    machine
        .channel()
        .send(event::Msg::Resize(WinsizeBuilder {
            cols: 60,
            rows: 20,
            width: 480,
            height: 320,
        }))
        .unwrap();

    machine
        .channel()
        .send(event::Msg::Input(b"later".to_vec().into()))
        .unwrap();

    assert!(matches!(
        machine.poll_io(&mut state, &mut buf, &mut cx),
        TaskPoll::Ready(Ok(true))
    ));
    assert!(
        machine
            .ghostty
            .format_text(None, false, true)
            .unwrap()
            .contains("output during resize")
    );
    assert!(machine.pty.writer.operations.is_empty());

    finish_resize.send(()).unwrap();

    assert!(matches!(
        machine.poll_io(&mut state, &mut buf, &mut cx),
        TaskPoll::Ready(Ok(true))
    ));
    assert_eq!(
        machine.pty.writer.operations,
        ["resize:60x20", "input:later"]
    );
}

#[test]
fn child_exit_retains_output_beyond_one_parse_batch() {
    let mut machine = resized_pipe(b"");
    let mut data = vec![b' '; 3 * READ_BUFFER_SIZE];

    data.extend_from_slice(b"\r\nfinal output");

    let expected_len = data.len();
    let observed = Arc::new(AtomicUsize::new(0));
    let count = observed.clone();

    machine.set_output_sink(move |bytes| {
        count.fetch_add(bytes.len(), Ordering::Relaxed);
    });

    machine.pty.reader.data = data;
    machine.pty.writer.exited = true;

    let runtime = Builder::new_current_thread().enable_all().build().unwrap();

    let (mut machine, _) = runtime.block_on(async {
        timeout(time::Duration::from_secs(2), machine.run_event_loop())
            .await
            .unwrap()
    });

    assert_eq!(observed.load(Ordering::Relaxed), expected_len);
    assert!(
        machine
            .ghostty
            .format_text(None, false, true)
            .unwrap()
            .contains("final output")
    );
    assert!(!machine.snapshot_pending);
}

#[test]
fn idle_async_loop_parks_until_a_command_arrives() {
    let machine = resized_pipe(b"");
    let sender = machine.channel();
    let polls = Arc::new(AtomicUsize::new(0));
    let counter = polls.clone();
    let runtime = paused_runtime();

    runtime.block_on(async {
        let task = tokio::spawn(async move {
            let mut future = pin!(machine.run_event_loop());

            poll_fn(|cx| {
                counter.fetch_add(1, Ordering::Relaxed);

                future.as_mut().poll(cx)
            })
            .await
        });

        sleep(time::Duration::from_millis(20)).await;

        let parked = polls.load(Ordering::Relaxed);

        assert!(parked > 0);

        sleep(time::Duration::from_secs(60)).await;

        assert_eq!(
            polls.load(Ordering::Relaxed),
            parked,
            "an idle PTY task woke itself"
        );

        sender
            .send(event::Msg::Input(b"wake".to_vec().into()))
            .unwrap();

        sleep(time::Duration::from_millis(20)).await;

        sender.send(event::Msg::Shutdown).unwrap();

        let (machine, _) = timeout(time::Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(machine.pty.writer.data, b"wake");
    });
}

#[test]
fn many_sessions_make_progress_on_one_async_worker() {
    let runtime = Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();

    let (tx, rx) = sync::mpsc::channel();

    let mut sessions = Vec::new();

    for id in 0..12 {
        let mut machine = resized_pipe(b"");

        machine.pty.writer.observer = Some(tx.clone());

        let sender = machine.channel();
        let task = runtime.spawn(machine.run_event_loop());

        sender.send(event::Msg::Input(vec![id].into())).unwrap();
        sessions.push((sender, task));
    }

    let mut received = Vec::new();

    for _ in 0..sessions.len() {
        received.extend(rx.recv_timeout(time::Duration::from_secs(2)).unwrap());
    }

    received.sort_unstable();

    assert_eq!(received, (0..12).collect::<Vec<u8>>());

    runtime.block_on(async {
        for (sender, task) in sessions {
            sender.send(event::Msg::Shutdown).unwrap();

            timeout(time::Duration::from_secs(2), task)
                .await
                .unwrap()
                .unwrap();
        }
    });
}

#[test]
fn delayed_resize_parks_then_rearms_the_input_deadline() {
    let mut machine = resized_pipe(b"");

    let sender = machine.channel();
    let (written, received) = sync::mpsc::channel();
    let (finish_resize, resized) = oneshot::channel();
    let polls = Arc::new(AtomicUsize::new(0));
    let counter = polls.clone();
    let runtime = paused_runtime();

    machine.pty.writer.observer = Some(written);
    machine.pty.writer.resize_completion = Some(resized);

    sender
        .send(event::Msg::PowerShellCompatibility(true))
        .unwrap();

    sender
        .send(event::Msg::Resize(WinsizeBuilder {
            cols: 60,
            rows: 20,
            width: 480,
            height: 320,
        }))
        .unwrap();

    sender
        .send(event::Msg::Input(b"after resize".to_vec().into()))
        .unwrap();

    runtime.block_on(async {
        let task = tokio::spawn(async move {
            let mut future = pin!(machine.run_event_loop());

            poll_fn(|cx| {
                counter.fetch_add(1, Ordering::Relaxed);

                future.as_mut().poll(cx)
            })
            .await
        });

        sleep(time::Duration::from_millis(20)).await;

        let parked = polls.load(Ordering::Relaxed);

        sleep(RESIZE_INPUT_DELAY + time::Duration::from_millis(20)).await;

        assert_eq!(
            polls.load(Ordering::Relaxed),
            parked,
            "input timeout spun during native resize"
        );
        assert!(received.try_recv().is_err());

        finish_resize.send(()).unwrap();

        sleep(RESIZE_INPUT_DELAY + time::Duration::from_millis(20)).await;

        assert_eq!(
            received
                .try_recv()
                .expect("resize completion did not rearm input"),
            b"after resize"
        );

        sender.send(event::Msg::Shutdown).unwrap();

        timeout(time::Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
    });
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

    assert_ne!(drain(&mut machine, &mut state), Step::Shutdown);
    assert_eq!(machine.ghostty.cols(), 80);

    machine.pty_write(&mut state, &mut noop_cx()).unwrap();

    assert_ne!(drain(&mut machine, &mut state), Step::Shutdown);
    assert_eq!(machine.ghostty.cols(), 80);
    assert_eq!(machine.pty.writer.data, b"ab");

    machine.pty.writer.budget = None;

    machine.pty_write(&mut state, &mut noop_cx()).unwrap();

    assert_ne!(drain(&mut machine, &mut state), Step::Shutdown);
    assert_eq!(machine.ghostty.cols(), 80);
    assert_eq!(machine.pty.writer.data, b"abc");

    machine.pty.writer.flush_pending = false;

    assert_ne!(drain(&mut machine, &mut state), Step::Shutdown);

    machine.pty_write(&mut state, &mut noop_cx()).unwrap();

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

    assert_ne!(drain(&mut machine, &mut state), Step::Shutdown);

    machine.pty_write(&mut state, &mut noop_cx()).unwrap();

    assert_ne!(drain(&mut machine, &mut state), Step::Shutdown);

    machine.pty_write(&mut state, &mut noop_cx()).unwrap();

    assert_eq!(
        machine.pty.writer.operations,
        ["resize:60x20", "input:A", "resize:100x30", "input:B"]
    );
}

#[test]
fn input_released_after_resize_wakes_an_otherwise_idle_loop() {
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

    let worker = nmt_runtime::handle().spawn(machine.run_event_loop());

    let first = received.recv_timeout(time::Duration::from_secs(2));
    let second = received.recv_timeout(time::Duration::from_secs(2));

    sender.send(event::Msg::Shutdown).unwrap();

    nmt_runtime::handle().block_on(worker).unwrap();

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

    assert_ne!(drain(&mut machine, &mut state), Step::Shutdown);

    machine.pty.reader.data.extend_from_slice(b"\x1b[6n");

    machine
        .pty_read(&mut state, &mut [0; READ_BUFFER_SIZE], &mut noop_cx())
        .unwrap();

    machine.pty_write(&mut state, &mut noop_cx()).unwrap();

    assert_eq!(machine.pty.writer.data, b"\x1b[4;5R");
    assert_eq!(machine.ghostty.cols(), 80);

    sender.send(event::Msg::Shutdown).unwrap();

    assert_eq!(drain(&mut machine, &mut state), Step::Shutdown);
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

    assert_ne!(drain(&mut machine, &mut state), Step::Shutdown);
    assert!(!state.needs_write());
    assert_eq!(machine.ghostty.cols(), 60);

    machine.pty.reader.data.extend_from_slice(b"\x1b[6n");

    machine
        .pty_read(&mut state, &mut [0; READ_BUFFER_SIZE], &mut noop_cx())
        .unwrap();

    machine.pty_write(&mut state, &mut noop_cx()).unwrap();

    assert_eq!(machine.pty.writer.data, b"\x1b[4;5R");

    sender
        .send(event::Msg::PowerShellCompatibility(false))
        .unwrap();

    assert_ne!(drain(&mut machine, &mut state), Step::Shutdown);

    machine.pty_write(&mut state, &mut noop_cx()).unwrap();

    assert_eq!(machine.ghostty.cols(), 60);
    assert_ne!(drain(&mut machine, &mut state), Step::Shutdown);

    machine.pty_write(&mut state, &mut noop_cx()).unwrap();

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

    assert_ne!(drain(&mut machine, &mut state), Step::Shutdown);

    sender.send(event::Msg::Shutdown).unwrap();

    assert_eq!(drain(&mut machine, &mut state), Step::Shutdown);
    assert_eq!(machine.pty.writer.data, b"\x1b[4;5RAB");
}

#[test]
fn powershell_input_deadline_wakes_a_quiet_event_loop() {
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

    let worker = nmt_runtime::handle().spawn(machine.run_event_loop());

    let bytes = received.recv_timeout(time::Duration::from_secs(2));
    let elapsed = started.elapsed();

    sender.send(event::Msg::Shutdown).unwrap();

    nmt_runtime::handle().block_on(worker).unwrap();

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

    assert_ne!(drain(&mut machine, &mut state), Step::Shutdown);

    sender
        .send(event::Msg::Resize(WinsizeBuilder {
            cols: 80,
            rows: 25,
            width: 640,
            height: 400,
        }))
        .unwrap();

    assert_ne!(drain(&mut machine, &mut state), Step::Shutdown);

    sender
        .send(event::Msg::Input(b"same-size".to_vec().into()))
        .unwrap();

    assert_ne!(drain(&mut machine, &mut state), Step::Shutdown);

    machine.pty_write(&mut state, &mut noop_cx()).unwrap();

    sender
        .send(event::Msg::Resize(WinsizeBuilder {
            cols: 60,
            rows: 20,
            width: 480,
            height: 360,
        }))
        .unwrap();

    assert_ne!(drain(&mut machine, &mut state), Step::Shutdown);

    machine.on_pty_chunk(b"\x1b[?1049h");

    sender
        .send(event::Msg::Input(b"alternate".to_vec().into()))
        .unwrap();

    assert_ne!(drain(&mut machine, &mut state), Step::Shutdown);

    machine.pty_write(&mut state, &mut noop_cx()).unwrap();

    sender
        .send(event::Msg::Resize(WinsizeBuilder {
            cols: 80,
            rows: 25,
            width: 640,
            height: 400,
        }))
        .unwrap();

    assert_ne!(drain(&mut machine, &mut state), Step::Shutdown);

    machine.on_pty_chunk(b"\x1b[?1049l");

    sender
        .send(event::Msg::Input(b"returned".to_vec().into()))
        .unwrap();

    assert_ne!(drain(&mut machine, &mut state), Step::Shutdown);

    machine.pty_write(&mut state, &mut noop_cx()).unwrap();

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

    let mut machine = Termio::new(
        Arc::new(FrameStore::new(RenderBuffer::new(20, 3))),
        Arc::new(AtomicU32::new(0)),
        pty,
        VoidListener {},
        &SessionOptions {
            cols: 20,
            rows: 3,
            route_id: 0,
            colors: Colors::default(),
            cursor_shape: CursorShape::Block,
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
        .pty_read(&mut state, &mut [0; READ_BUFFER_SIZE], &mut noop_cx())
        .unwrap();

    assert!(machine.pty.writer.data.is_empty());

    machine.pty_write(&mut state, &mut noop_cx()).unwrap();

    assert_eq!(machine.pty.writer.data, b"in");
    assert!(state.needs_write());

    machine.pty.writer.budget = Some(3);

    machine.pty_write(&mut state, &mut noop_cx()).unwrap();

    assert_eq!(machine.pty.writer.data, b"in\x1b[1");
    assert!(state.needs_write());

    machine.pty.writer.budget = None;

    machine.pty_write(&mut state, &mut noop_cx()).unwrap();

    assert_eq!(machine.pty.writer.data, b"in\x1b[1;1R");
    assert!(!state.needs_write());
}

#[test]
fn explicit_session_shutdown_releases_worker_resources_before_returning() {
    assert_session_cleanup(|handles| block_on(handles.shutdown()));
}

#[test]
fn shared_session_shutdown_waits_for_cleanup_from_a_tokio_task() {
    assert_session_cleanup(|handles| {
        let runtime = nmt_runtime::handle();
        let handles = Arc::new(handles);

        runtime
            .block_on(runtime.spawn(async move {
                Arc::into_inner(handles)
                    .expect("sole session owner")
                    .shutdown()
                    .await
            }))
            .unwrap();
    });
}

#[test]
fn session_shutdown_waits_for_cleanup_from_a_futures_executor() {
    assert_session_cleanup(|handles| {
        block_on(handles.shutdown());
    });
}

fn assert_session_cleanup(close: impl FnOnce(SessionHandles)) {
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
            cursor_shape: CursorShape::Block,
            scrollback_lines: 1000,
            engine_blocks: false,
            terminal_responses: true,
            output_sink: Some(Arc::new(move |_| {
                let _ = &lifetime;
            })),
        },
    )
    .unwrap();

    close(handles);

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

    let mut machine = Termio::new(
        Arc::new(FrameStore::new(RenderBuffer::new(20, 3))),
        Arc::new(AtomicU32::new(0)),
        pty,
        VoidListener {},
        &SessionOptions {
            cols: 20,
            rows: 3,
            route_id: 0,
            colors: Colors::default(),
            cursor_shape: CursorShape::Block,
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
        .pty_read(
            &mut PtyState::default(),
            &mut [0; READ_BUFFER_SIZE],
            &mut noop_cx(),
        )
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

    let mut machine = Termio::new(
        Arc::clone(&render_buffer),
        vt_modes,
        pty,
        VoidListener {},
        &SessionOptions {
            cols: 20,
            rows: 3,
            route_id: 0,
            colors: Colors::default(),
            cursor_shape: CursorShape::Block,
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

    assert_ne!(drain(&mut machine, &mut state), Step::Shutdown);

    let buffer = render_buffer.load();

    assert_eq!(buffer.rows(), 5);
    assert_eq!(render_buffer_row_text(&buffer, 0), "resize-ok");
}

#[test]
fn theme_refresh_preserves_synchronized_output_until_commit() {
    let render_buffer = Arc::new(FrameStore::new(RenderBuffer::new(20, 3)));

    let pty = FakePty {
        reader: FakeReader {
            data: b"\x1b[3;1H> input\x1b[3;3H".to_vec(),
        },
        writer: FakeWriter::default(),
    };

    let mut machine = Termio::new(
        Arc::clone(&render_buffer),
        Arc::new(AtomicU32::new(0)),
        pty,
        VoidListener {},
        &SessionOptions {
            cols: 20,
            rows: 3,
            route_id: 0,
            colors: Colors::default(),
            cursor_shape: CursorShape::Block,
            scrollback_lines: 1000,
            engine_blocks: false,
            terminal_responses: true,
            output_sink: None,
        },
    )
    .unwrap();

    let colors = Colors {
        foreground: [0.2, 0.4, 0.6, 1.0],
        ..Colors::default()
    };

    let mut state = PtyState::default();
    let mut buf = [0u8; READ_BUFFER_SIZE];

    machine
        .pty_read(&mut state, &mut buf, &mut noop_cx())
        .unwrap();

    assert_eq!(render_buffer.load().cursor().row.0, 2);

    machine
        .pty
        .reader
        .data
        .extend_from_slice(b"\x1b[?2026h\x1b[1;1HWorking");

    machine
        .pty_read(&mut state, &mut buf, &mut noop_cx())
        .unwrap();

    machine
        .sender
        .send(event::Msg::Theme(Box::new(colors)))
        .unwrap();

    assert_ne!(drain(&mut machine, &mut state), Step::Shutdown);

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

    machine
        .pty_read(&mut state, &mut buf, &mut noop_cx())
        .unwrap();

    {
        let buffer = render_buffer.load();

        assert_eq!(buffer.cursor().row.0, 2);
        assert_eq!(render_buffer_row_text(&buffer, 0), "Working");
        assert_eq!(
            buffer.colors()[NamedColor::Foreground],
            Some(colors.foreground)
        );
    }

    machine
        .pty
        .reader
        .data
        .extend_from_slice(b"\x1b[?2026h\x1b[2;1HStuck");

    machine
        .pty_read(&mut state, &mut buf, &mut noop_cx())
        .unwrap();

    machine.sync_output_started_at = Some(Instant::now() - SYNC_OUTPUT_TIMEOUT);

    machine
        .pty_read(&mut state, &mut buf, &mut noop_cx())
        .unwrap();

    {
        let buffer = render_buffer.load();

        assert_eq!(buffer.cursor().row.0, 1);
        assert_eq!(render_buffer_row_text(&buffer, 1), "Stuck");
    }

    assert!(!machine.ghostty.mode(mode::SYNC_OUTPUT));
}

#[test]
fn progress_cursor_suppression_keeps_layout_and_resets_on_capture() {
    let mut engine = ghostty::GhosttyTerminal::new(20, 3, 100).unwrap();
    let mut buffer = RenderBuffer::new(20, 3);

    engine.write_vt(b"\x1b[3;1H");
    engine.snapshot_into(&mut buffer, 0, 0).unwrap();
    buffer.suppress_progress_cursor();

    assert!(!buffer.cursor_visible());
    assert_eq!(buffer.layout_cursor_row(), Some(2));

    engine.snapshot_into(&mut buffer, 0, 0).unwrap();

    assert!(
        buffer.cursor_visible(),
        "reused buffers clear host suppression"
    );
    assert_eq!(buffer.layout_cursor_row(), Some(2));

    engine.write_vt(b"\x1b[?25l");
    engine.snapshot_into(&mut buffer, 0, 0).unwrap();
    buffer.suppress_progress_cursor();

    assert!(!buffer.cursor_visible());
    assert_eq!(buffer.layout_cursor_row(), None);

    engine.write_vt(b"\x1b[?25h\r\n\r\n\r\nPrompt>");
    engine.scroll_viewport_top();
    engine.snapshot_into(&mut buffer, 0, 0).unwrap();
    buffer.suppress_progress_cursor();

    assert!(!buffer.cursor_visible());
    assert_eq!(buffer.layout_cursor_row(), None);
}

#[test]
fn theme_refresh_preserves_progress_cursor_suppression() {
    let render_buffer = Arc::new(FrameStore::new(RenderBuffer::new(80, 3)));

    let pty = FakePty {
        reader: FakeReader {
            data: b"\x1b]9;4;1".to_vec(),
        },
        writer: FakeWriter::default(),
    };

    let mut machine = Termio::new(
        Arc::clone(&render_buffer),
        Arc::new(AtomicU32::new(0)),
        pty,
        VoidListener {},
        &SessionOptions {
            cols: 80,
            rows: 3,
            route_id: 0,
            colors: Colors::default(),
            cursor_shape: CursorShape::Block,
            scrollback_lines: 1000,
            engine_blocks: false,
            terminal_responses: true,
            output_sink: None,
        },
    )
    .unwrap();

    machine
        .ghostty
        .set_default_cursor_shape(CursorShape::Beam)
        .unwrap();

    let colors = Colors {
        foreground: [0.2, 0.4, 0.6, 1.0],
        ..Colors::default()
    };

    let mut state = PtyState::default();
    let mut buf = [0u8; READ_BUFFER_SIZE];

    machine
        .pty_read(&mut state, &mut buf, &mut noop_cx())
        .unwrap();

    machine
        .pty
        .reader
        .data
        .extend_from_slice(b";42\x1b\\    Building [====>     ] 4/10\r");

    machine.last_snapshot_at = Some(Instant::now() - SNAPSHOT_MIN_INTERVAL);

    machine
        .pty_read(&mut state, &mut buf, &mut noop_cx())
        .unwrap();

    machine
        .sender
        .send(event::Msg::Theme(Box::new(colors)))
        .unwrap();

    assert_ne!(drain(&mut machine, &mut state), Step::Shutdown);

    {
        let buffer = render_buffer.load();

        assert_eq!(buffer.cursor_shape(), CursorShape::Beam);
        assert!(!buffer.cursor_visible(), "active progress hides the cursor");
        assert_eq!(buffer.layout_cursor_row(), Some(0));
        assert_eq!(
            buffer.colors()[NamedColor::Foreground],
            Some(colors.foreground)
        );
    }

    machine
        .pty
        .reader
        .data
        .extend_from_slice(b"\x1b]9;4;0;\x1b\\");

    machine.last_snapshot_at = Some(Instant::now() - SNAPSHOT_MIN_INTERVAL);

    machine
        .pty_read(&mut state, &mut buf, &mut noop_cx())
        .unwrap();

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

    let mut machine = Termio::new(
        render_buffer,
        vt_modes,
        pty,
        VoidListener {},
        &SessionOptions {
            cols: 134,
            rows: 42,
            route_id: 0,
            colors: Colors::default(),
            cursor_shape: CursorShape::Block,
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

    machine
        .pty_read(&mut state, &mut read_buf, &mut noop_cx())
        .unwrap();

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

    let mut machine = Termio::new(
        render_buffer,
        vt_modes,
        pty,
        VoidListener {},
        &SessionOptions {
            cols: 20,
            rows: 4,
            route_id: 0,
            colors: Colors::default(),
            cursor_shape: CursorShape::Block,
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

    machine
        .pty_read(&mut state, &mut read_buf, &mut noop_cx())
        .unwrap();

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
    Termio<FakePty, CollectingListener>,
) {
    let events = Arc::new(Mutex::new(Vec::new()));

    let pty = FakePty {
        reader: FakeReader {
            data: stream.to_vec(),
        },
        writer: FakeWriter::default(),
    };

    let mut machine = Termio::new(
        Arc::new(FrameStore::new(RenderBuffer::new(80, 24))),
        Arc::new(AtomicU32::new(0)),
        pty,
        CollectingListener(Arc::clone(&events)),
        &SessionOptions {
            cols: 80,
            rows: 24,
            route_id: 0,
            colors: Colors::default(),
            cursor_shape: CursorShape::Block,
            scrollback_lines: 1000,
            engine_blocks: true,
            terminal_responses: true,
            output_sink: None,
        },
    )
    .unwrap();

    let mut state = PtyState::default();
    let mut buf = [0u8; READ_BUFFER_SIZE];

    machine
        .pty_read(&mut state, &mut buf, &mut noop_cx())
        .unwrap();

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
fn prediction_cleanup_cannot_discard_completed_output() {
    let stream = b"\x1b]133;A\x07\x1b]133;B\x07\x1b]133;C\x07\x1b]133;D;0\x07\
\x1b]133;A\x07PS> \x1b]133;B\x07\x1b[1;5Hls\
\x1b[2;1H\x1b[K\x1b[1;7H\r\n\x1b]133;C\x07Cargo.toml\r\n\
\x1b]133;D;0\x07\x1b[2J\x1b[3J\x1b[H\x1b]133;A\x07PS> \x1b]133;B\x07";

    let (events, machine) = pty_read_events(stream);

    assert_eq!(machine.ghostty.block_count(), 1);

    let handle = machine.ghostty.block_at(0).unwrap();
    let rows = machine.ghostty.block_row_count(handle).unwrap();

    let mut text = String::new();

    for row in 0..rows {
        for cell in machine
            .ghostty
            .read_block_row(handle, row)
            .unwrap()
            .unwrap()
            .cells
        {
            text.push_str(&cell.text);
        }
    }

    assert!(text.contains("Cargo.toml"));
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, event::TerminalEvent::CommandFinished(_)))
            .count(),
        1
    );
}

/// The engine must accept a `;C` mark that carries parameters, or the output
/// rows lose their semantic tag and the block disappears.
#[test]
fn submitted_command_survives_prediction_cleanup() {
    let stream = b"\x1b]133;A\x07\x1b]133;B\x07\x1b]133;C\x07\x1b]133;D;0\x07\
\x1b]133;A\x07PS> \x1b]133;B\x07\x1b[1;5Hls\
\x1b[2;1H\x1b[K\x1b[1;7H\r\n\x1b]133;C;cmdline=bHM=\x07\
Cargo.toml\r\n\x1b]133;D;0\x07\x1b[2J\x1b[3J\x1b[H";

    let (events, machine) = pty_read_events(stream);

    let finished: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            event::TerminalEvent::CommandFinished(c) => Some(c),
            _ => None,
        })
        .collect();

    assert_eq!(finished.len(), 1);
    assert_eq!(finished[0].command.as_deref(), Some("ls"));
    assert_eq!(machine.ghostty.block_count(), 1);
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

    assert_eq!(b.command.as_deref(), Some("cd dest"));
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
                    BlockEvent::EngineBlock {
                        seq, rows, handle, ..
                    } => {
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
    use crate::event::{BlockEvent, TerminalEvent};

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
    assert_eq!(starts[0].command.as_deref(), Some("echo one"));
    assert_eq!(starts[1].command.as_deref(), Some("echo two"));
    assert_eq!(finishes[0].command.as_deref(), Some("echo one"));
    assert_eq!(finishes[1].command.as_deref(), Some("echo two"));
    assert_eq!(starts[0].seq, finishes[0].seq);
    assert_eq!(starts[1].seq, finishes[1].seq);
    assert!(starts[0].seq < starts[1].seq);

    // Each frozen block carries the complete command record, so the store
    // never has to join metadata by sequence number later.
    let blocks: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            TerminalEvent::BlockBatch(batch) => Some(batch),
            _ => None,
        })
        .flatten()
        .filter_map(|b| match b {
            BlockEvent::EngineBlock { seq, meta, .. } => Some((*seq, meta.clone())),
            _ => None,
        })
        .collect();

    assert_eq!(blocks.len(), 2, "one frozen block per real command");

    for ((seq, meta), finish) in blocks.iter().zip(&finishes) {
        assert_eq!(*seq, finish.seq);
        assert_eq!(meta.command, finish.command);
        assert_eq!(meta.exit_code, Some(0));
        assert_eq!(meta.started_at, Some(finish.started_at));
        assert_eq!(meta.ended_at, Some(finish.ended_at));
        assert!(
            meta.cwd.as_deref().is_some_and(|cwd| cwd.ends_with('w')),
            "launch cwd rides with the block, got {:?}",
            meta.cwd
        );
    }

    // Mark forwarding is working when the engine tags the
    // prompt rows — the drift-correction ground truth for the view.
    assert!(
        machine.ghostty.has_prompt_tagged_row(),
        "engine rows must carry semantic prompt tags after mark forwarding"
    );
}

#[test]
fn idle_theme_requests_publish_colors_without_replacing_retained_frames() {
    let (_, mut machine) = pty_read_events(b"idle text");

    let original = machine.render_buffer.load();

    let mut state = PtyState::default();

    for (foreground, background) in [
        ([0.2, 0.4, 0.6, 1.0], [1.0, 1.0, 1.0, 1.0]),
        ([1.0, 1.0, 1.0, 1.0], [0.0, 0.0, 0.0, 1.0]),
    ] {
        let colors = Colors {
            foreground,
            background,
            ..Colors::default()
        };

        machine.event_proxy.0.lock().clear();

        machine
            .sender
            .send(event::Msg::Theme(Box::new(colors)))
            .unwrap();

        assert_ne!(drain(&mut machine, &mut state), Step::Shutdown);

        let frame = machine.render_buffer.load();

        assert_eq!(frame.colors()[NamedColor::Foreground], Some(foreground));
        assert_eq!(frame.colors()[NamedColor::Background], Some(background));
        assert_eq!(render_buffer_row_text(&frame, 0), "idle text");
        assert!(
            machine
                .event_proxy
                .0
                .lock()
                .iter()
                .any(|event| { matches!(event, event::TerminalEvent::TerminalDamaged(_)) })
        );
        assert!(!Arc::ptr_eq(&original, &frame));
    }

    assert_eq!(render_buffer_row_text(&original, 0), "idle text");
}
