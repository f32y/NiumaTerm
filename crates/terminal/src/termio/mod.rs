pub use crate::termio::session::{
    OutputSink, SessionHandles, SessionOptions, SessionWorker, start_session,
};
pub use crate::termio::write_queue::PtyState;

pub(crate) mod requests;

mod marks;
mod powershell_compatibility;
mod prompt_sniffer;
mod session;
mod vt_trace;
mod write_queue;

#[cfg(test)]
mod ghostty_mirror_tests;

use std::collections::VecDeque;
use std::future::poll_fn;
use std::io::{self, ErrorKind};
use std::pin::pin;
use std::sync::atomic::AtomicU32;
use std::sync::{self, Arc};
use std::task::{Context, Poll as TaskPoll};
use std::{cell, error, path, time};

use nmt_platform::{AsyncPty, WinsizeBuilder};
#[cfg(enable_profiling)]
use nmt_profiling::pty::{BatchEnd, PtyProfiler, Stage};
use tokio::sync::mpsc::{self, UnboundedReceiver};
use tokio::task::yield_now;
use tokio::time::{Instant, sleep_until};
use tracing::{error, warn};

use crate::event::{self, Checkpoint, EventListener, Msg, MsgSender, RequestError, TerminalEvent};
use crate::ghostty::{self, GhosttyTerminal, mode};
use crate::render_buffer::{FrameStore, RenderBuffer};
use crate::termio::marks::{apply_sniffer_mark, engine_blocks_live_list};
use crate::termio::powershell_compatibility::PowerShellCompatibility;
use crate::termio::prompt_sniffer::PromptSniffer;
use crate::termio::requests::answer_query;
use crate::vt_modes;

const READ_BUFFER_SIZE: usize = 64 * 1024;

/// Yield to queued commands after each bounded PTY read batch.
const MAX_READ_BATCH: usize = u16::MAX as usize;

/// Coalesce viewport captures across PTY batches, including short bursts that
/// temporarily drain the pipe. Five milliseconds fits within a 144 Hz frame;
/// the poll deadline publishes the final burst even when no more bytes arrive.
/// Output after an idle interval is captured immediately.
const SNAPSHOT_MIN_INTERVAL: time::Duration = time::Duration::from_millis(5);

/// Match Windows Terminal's upper bound so a missing DEC 2026 reset cannot
/// leave the last committed frame visible indefinitely.
const SYNC_OUTPUT_TIMEOUT: time::Duration = time::Duration::from_millis(100);

/// A size change handed to the PTY and not yet completed. Input waits behind
/// it, and the PowerShell pause starts only once it has finished.
#[derive(Clone)]
struct ResizeInFlight {
    size: WinsizeBuilder,
    grid_changed: bool,
}

/// What one pass over queued commands did, so the owner task knows whether to
/// poll again before parking.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Idle,
    Advanced,
    Shutdown,
}

#[derive(Clone, Copy)]
enum FlushReason {
    Drained,
    Saturated,
    Command,
    Exit,
}

/// The PTY task's state and the engine's single owner. It reads PTY output
/// into the engine, forwards input and resizes to the PTY, tracks the shell
/// lifecycle marks, publishes captured frames on a bounded cadence, and
/// answers the session's asynchronous reads. Named after Ghostty's `Termio`,
/// which holds the same role.
pub struct Termio<T: AsyncPty, U: EventListener> {
    sender: MsgSender,
    receiver: UnboundedReceiver<Msg>,

    /// Inputs carry a fixed wait limit measured at receipt; other commands carry None.
    pending_commands: VecDeque<(Msg, Option<Instant>)>,

    pending_resize: Option<ResizeInFlight>,

    powershell_compatibility: PowerShellCompatibility,
    pty: T,
    child_exited: bool,

    /// The event loop exclusively owns the parser and all mutable engine state.
    /// Other threads submit commands and retain published data independently.
    ghostty: GhosttyTerminal,

    theme_revision: u64,

    /// Publication exchanges immutable frame ownership without exposing the
    /// engine. Readers release the short publication lock before extraction.
    render_buffer: Arc<FrameStore>,

    /// PTY-task-private target for direct Ghostty capture. A completed frame
    /// swaps with `render_buffer`, so the shared lock covers only publication.
    back_buffer: RenderBuffer,

    capture_failed: bool,

    /// VT modes published to the frontend. This `Termio` is the sole writer;
    /// the input path reads it lock-free. `Mode` is `u32`.
    vt_modes: Arc<AtomicU32>,

    /// Monotonic content version, bumped once per PTY batch / resize (the engine
    /// exposes no generation signal). The frontend's deep-search corpus cache
    /// compares it to detect content changes and invalidate cached views.
    content_version: u64,

    /// Optional observer for the exact VT stream accepted by the engine. It runs
    /// in the owner task before another command or byte batch can run,
    /// preserving checkpoint ordering. Observers must return promptly.
    output_sink: Option<OutputSink>,

    terminal_responses_enabled: bool,
    event_proxy: U,
    route_id: usize,

    /// OSC 133 region state; only touched on the PTY task.
    sniffer: PromptSniffer,

    /// Launch cwd of the currently-running command, latched at its `;C`: the cwd MUST
    /// NOT be read at `;D`, by which point the ps1 has already reported the NEXT
    /// prompt's OSC 7 directory (which would mislabel every `cd`). Attached to the
    /// CommandFinished event. PTY-task-private.
    launch_cwd: Option<Option<path::PathBuf>>,

    /// Engine-blocks mode is the default: at
    /// each trusted `;D` the engine freezes the command into a finished
    /// block (`finish_block`, O(1)) whose handle is shipped to the app's
    /// block store; rendering reads the frozen block directly. `false` is
    /// the classic single-grid fallback: no finish, no boundary
    /// clear, no block events — plain terminal behavior.
    engine_blocks: bool,

    /// OSC 133 prompt-boundary sequence number; incremented at each trusted
    /// `;A` and stamped onto Command* events for segment/metadata marriage.
    mark_seq: u64,

    /// Last-seen alt-screen state, for edge-triggered interactive-state events.
    prev_alt_screen: bool,

    /// Last value sent by both alt-screen notifications, which share one edge.
    prev_alt_screen_sent: bool,

    /// The last capture time; absent until the first PTY output is captured.
    last_snapshot_at: Option<Instant>,

    /// True when a recent capture or synchronized update deferred its readback.
    /// Makes the event loop run `pty_read` without requiring new PTY bytes.
    snapshot_pending: bool,

    /// Start of the current DEC 2026 transaction. The event-loop poll uses this
    /// deadline to recover when an application omits the matching reset.
    sync_output_started_at: Option<Instant>,

    #[cfg(enable_profiling)]
    profile: PtyProfiler,
}

/// Read the VT-controlled modes from the Ghostty engine into `Mode`.
/// bits (e.g. `Mode::VI`) are not touched here — see
/// [`Crosswords::sync_vt_modes`].
fn ghostty_vt_modes(g: &GhosttyTerminal) -> vt_modes::Mode {
    use crate::ghostty::mode as gm;
    use crate::vt_modes::Mode;

    let mut m = Mode::empty();

    m.set(Mode::SHOW_CURSOR, g.mode(gm::CURSOR_VISIBLE));

    m.set(Mode::APP_CURSOR, g.mode(gm::CURSOR_KEYS));

    m.set(Mode::APP_KEYPAD, g.mode(gm::KEYPAD_KEYS));

    m.set(Mode::MOUSE_REPORT_CLICK, g.mode(gm::MOUSE_NORMAL));

    m.set(Mode::MOUSE_DRAG, g.mode(gm::MOUSE_BUTTON));

    m.set(Mode::MOUSE_MOTION, g.mode(gm::MOUSE_ANY));

    m.set(Mode::SGR_MOUSE, g.mode(gm::MOUSE_SGR));

    m.set(Mode::UTF8_MOUSE, g.mode(gm::MOUSE_UTF8));

    m.set(Mode::ALTERNATE_SCROLL, g.mode(gm::MOUSE_ALTERNATE_SCROLL));

    m.set(Mode::BRACKETED_PASTE, g.mode(gm::BRACKETED_PASTE));

    m.set(Mode::FOCUS_IN_OUT, g.mode(gm::FOCUS_EVENT));

    m.set(Mode::LINE_WRAP, g.mode(gm::WRAPAROUND));

    m.set(Mode::INSERT, g.mode(gm::INSERT));

    m.set(Mode::ALT_SCREEN, g.mode(gm::ALT_SCREEN));

    // Kitty keyboard protocol flags live in a separate engine stack, not the DEC
    // modes, so fold them in here to enable Kitty press and key-release encoding.
    m |= g.kitty_keyboard_modes();

    m
}

fn publish_render_buffer(
    front: &FrameStore,
    back: &mut RenderBuffer,
    capture: ghostty::Result<()>,
    hide_cursor: bool,
    capture_failed: &mut bool,
) -> bool {
    if let Err(error) = capture {
        if !*capture_failed {
            warn!("failed to capture terminal frame: {error}");
        }

        *capture_failed = true;

        return false;
    }

    if hide_cursor {
        back.suppress_progress_cursor();
    }

    *capture_failed = false;

    front.publish(back);

    true
}

/// Convert `scrollback-history-limit` (in **lines**) to the engine's
/// scrollback byte budget. The engine stores rows in byte-bounded pages
/// (~14 B/cell observed), so we size for `cols × 16 B/cell` — 16 is an upper bound,
/// guaranteeing *at least* `lines` rows at the creation width. Approximate by
/// nature: a later resize-widen can hold fewer than `lines` rows, and the engine
/// floors very small budgets at ~one page. `lines == 0` → 0 (engine minimum).
fn scrollback_bytes(lines: usize, cols: u16) -> usize {
    const BYTES_PER_CELL: usize = 16;

    lines
        .saturating_mul(cols as usize)
        .saturating_mul(BYTES_PER_CELL)
}

impl<T, U> Termio<T, U>
where
    T: AsyncPty + Send + 'static,
    U: EventListener + Send + 'static,
{
    pub(crate) fn new(
        render_buffer: Arc<FrameStore>,
        vt_modes: Arc<AtomicU32>,
        pty: T,
        event_proxy: U,
        options: &SessionOptions,
    ) -> Result<Termio<T, U>, Box<dyn error::Error>> {
        let (sender, receiver) = mpsc::unbounded_channel();

        // Start the engine at the render buffer's viewport dimensions so the first
        // resize cannot diverge from a zero-sized construction.
        let (cols, rows) = {
            let rb = render_buffer.load();

            (rb.cols() as u16, rb.rows() as u16)
        };

        // `max_scrollback` is a **byte budget** in the engine — the C-binding's
        // "lines" doc is wrong (verified: budgets ≤1 MB floor at ~3297 lines, 10 MB
        // holds ~36k at 20 cols ≈ 273 B/line). `scrollback-history-limit` is
        // in lines, so convert through `scrollback_bytes`.
        let max_scrollback = scrollback_bytes(options.scrollback_lines, cols.max(1));

        let mut ghostty = GhosttyTerminal::new(cols.max(1), rows.max(1), max_scrollback)
            .map_err(|err| Box::new(err) as Box<dyn error::Error>)?;

        // Push the host theme's default colors + 256-palette into the engine so
        // SGR-indexed and default colors resolve to theme.
        ghostty.set_theme_colors(&options.colors);

        // Seed the atomic with the engine's initial VT modes (SHOW_CURSOR,
        // LINE_WRAP, …) so the facade is correct before the first PTY batch.
        vt_modes.store(
            ghostty_vt_modes(&ghostty).bits(),
            sync::atomic::Ordering::Relaxed,
        );

        Ok(Termio {
            sender,
            receiver,
            pending_commands: VecDeque::new(),
            pending_resize: None,
            powershell_compatibility: PowerShellCompatibility::default(),
            pty,
            child_exited: false,
            ghostty,
            theme_revision: 0,
            render_buffer,
            capture_failed: false,
            back_buffer: RenderBuffer::new(cols as usize, rows as usize),
            vt_modes,
            content_version: 0,
            output_sink: None,
            terminal_responses_enabled: true,
            event_proxy,
            route_id: options.route_id,
            sniffer: PromptSniffer::default(),
            launch_cwd: None,
            engine_blocks: options.engine_blocks,
            mark_seq: 0,
            prev_alt_screen: false,
            prev_alt_screen_sent: false,
            last_snapshot_at: None,
            snapshot_pending: false,
            sync_output_started_at: None,
            #[cfg(enable_profiling)]
            profile: PtyProfiler::new(0, options.route_id, cols, rows),
        })
    }

    /// Emit alt-screen interactive state, edge-triggered so a stable state emits nothing.
    fn emit_interactive_state(&mut self) {
        let on = self.prev_alt_screen;

        if on != self.prev_alt_screen_sent {
            self.prev_alt_screen_sent = on;

            self.event_proxy
                .send_event(TerminalEvent::InteractiveState(on));

            self.event_proxy.send_event(TerminalEvent::AltScreen(on));
        }
    }

    /// Observe parsed VT output without taking ownership of the PTY loop.
    #[cfg(test)]
    pub(crate) fn set_output_sink(&mut self, sink: impl Fn(Arc<[u8]>) + Send + Sync + 'static) {
        self.output_sink = Some(Arc::new(sink));
    }

    /// Choose whether VT-generated DA/DSR/OSC replies are written to this PTY.
    #[cfg(test)]
    pub(crate) fn set_terminal_responses_enabled(&mut self, enabled: bool) {
        self.terminal_responses_enabled = enabled;
    }

    #[inline]
    fn pty_read(
        &mut self,
        state: &mut PtyState,
        buf: &mut [u8],
        cx: &mut Context<'_>,
    ) -> io::Result<usize> {
        let mut unprocessed = 0;
        let mut processed = 0;

        // True when the loop drained the PTY (WouldBlock/EOF); false when it
        // broke at MAX_READ_BATCH with more data likely pending.
        let mut caught_up = false;

        loop {
            // Read from the PTY.
            #[cfg(enable_profiling)]
            let read_started = self.profile.start();

            let read = match self.pty.poll_read(cx, &mut buf[unprocessed..]) {
                TaskPoll::Ready(result) => result,
                TaskPoll::Pending => Ok(0),
            };

            #[cfg(enable_profiling)]
            self.profile
                .read(read_started, read.as_ref().copied().unwrap_or(0));

            match read {
                // This is received on Windows/macOS when no more data is readable from the PTY.
                Ok(0) if unprocessed == 0 => {
                    caught_up = true;

                    break;
                }
                Ok(got) => unprocessed += got,
                Err(err) => return Err(err),
            }

            self.on_pty_chunk(&buf[..unprocessed]);

            // Preserve the last parsed bytes even if the next read reports EOF.
            self.snapshot_pending = true;

            // Content changed, so invalidate the cached deep-search corpus.
            self.content_version = self.content_version.wrapping_add(1);

            processed += unprocessed;
            unprocessed = 0;

            // Don't accumulate unboundedly before reflecting to the renderer.
            if processed >= MAX_READ_BATCH {
                break;
            }
        }

        let responses = self.ghostty.take_pty_writes();

        if self.terminal_responses_enabled && !responses.is_empty() {
            state.write_list.push_back(responses.into());
        }

        if processed == 0 && !self.snapshot_pending {
            return Ok(0);
        }

        #[cfg(enable_profiling)]
        let flush_started = {
            self.profile.batch(
                if caught_up {
                    BatchEnd::Drained
                } else {
                    BatchEnd::Saturated
                },
                processed,
            );

            self.profile.start()
        };

        let result = self.flush_engine_state(if caught_up {
            FlushReason::Drained
        } else {
            FlushReason::Saturated
        });

        #[cfg(enable_profiling)]
        self.profile.record(Stage::Flush, flush_started);

        result.map(|()| processed)
    }

    #[inline]
    fn on_pty_chunk(&mut self, input: &[u8]) {
        #[cfg(enable_profiling)]
        let ingest_started = self.profile.start();

        // The owner parses into private engine state while the UI retains its
        // last published frame. Neither side waits for the other's read pass.
        let output_sink = self.output_sink.clone();

        let engine = &mut self.ghostty;

        // ConPTY tracks both cursor and content for incremental redraws.
        // Altering addresses or inserting scrolls would change state it does
        // not know to repaint, so output reaches the engine unchanged.
        let observed_output = output_sink.as_ref().map(|_| input.into());

        // Always run the sniffer: it classifies/captures OSC 133 lifecycle state
        // while every byte, including marks, still reaches the engine.

        // Both feed closures need the engine (segment/mark forwarding
        // writes; cwd latch reads) and never run at the same time, so a
        // RefCell shares the one borrow.
        let engine_cell = cell::RefCell::new(&mut *engine);
        let launch_cwd = &mut self.launch_cwd;
        let mark_seq = &mut self.mark_seq;
        let event_proxy = &self.event_proxy;

        let engine_blocks = self.engine_blocks;

        self.sniffer.feed_hooked(
            input,
            |_, _, seg| {
                engine_cell.borrow_mut().write_vt(seg);
            },
            |mark| {
                apply_sniffer_mark(
                    &engine_cell,
                    launch_cwd,
                    mark_seq,
                    event_proxy,
                    engine_blocks,
                    mark,
                );
            },
        );

        if let Some(trusted) = self.sniffer.take_boundary_trust_changed() {
            self.event_proxy
                .send_event(TerminalEvent::PromptBoundaryTrusted(trusted));
        }

        // No steady-state work per read: the whole command is
        // frozen once at its `;D` finish (the per-line harvest
        // this replaced was the throughput cost the per-block
        // grid exists to delete).

        vt_trace::trace_read(self.route_id, engine, input);

        if let (Some(sink), Some(output)) = (&output_sink, observed_output) {
            sink(output);
        }

        #[cfg(enable_profiling)]
        self.profile.record(Stage::Ingest, ingest_started);
    }

    #[inline]
    fn flush_engine_state(&mut self, reason: FlushReason) -> io::Result<()> {
        // An empty pipe can be a gap between producer writes, not the end of
        // output. Both drained and saturated reads share the capture interval.
        // Explicit view commands and shutdown still publish immediately.
        let capture_due = match reason {
            FlushReason::Command | FlushReason::Exit => true,
            FlushReason::Drained | FlushReason::Saturated => self
                .last_snapshot_at
                .is_none_or(|at| at.elapsed() >= SNAPSHOT_MIN_INTERVAL),
        };

        // Collect one batch's metadata, image changes,
        // and frame before delivering events that announce the publication.
        let pwd = self.ghostty.poll_pwd();

        let (bell, clipboard_writes, progress, title, vt_modes, sync_output, capture, image_delta) = {
            let engine = &mut self.ghostty;

            let bell = engine.take_bell();
            let clipboard_writes = engine.take_clipboard_writes();
            let progress = engine.take_progress_report();
            let title = engine.poll_title();
            let vt_modes = ghostty_vt_modes(engine);

            let sync_output_timed_out = self
                .sync_output_started_at
                .is_some_and(|started| started.elapsed() >= SYNC_OUTPUT_TIMEOUT);

            if sync_output_timed_out && engine.mode(mode::SYNC_OUTPUT) {
                engine.write_vt(b"\x1b[?2026l");
            }

            let sync_output = engine.mode(mode::SYNC_OUTPUT);

            // Finishing a synchronized update commits its complete frame even
            // when the preceding publication was recent.
            let sync_finished = self.sync_output_started_at.is_some() && !sync_output;

            let (capture, image_delta) = if (capture_due || sync_finished) && !sync_output {
                #[cfg(enable_profiling)]
                let capture_started = self.profile.start();

                let capture = engine.snapshot_into(
                    &mut self.back_buffer,
                    self.content_version,
                    self.theme_revision,
                );

                #[cfg(enable_profiling)]
                self.profile.record(
                    match reason {
                        FlushReason::Saturated => Stage::CaptureSaturated,
                        _ => Stage::CaptureEager,
                    },
                    capture_started,
                );

                // Pixel updates describe the same engine state as the captured
                // placements and reach the frontend before that frame is announced.
                let image_delta = match &capture {
                    Ok(()) => engine.take_image_deltas(self.back_buffer.placements()),
                    Err(_) => (Vec::new(), Vec::new()),
                };

                (Some(capture), image_delta)
            } else {
                (None, (Vec::new(), Vec::new()))
            };

            (
                bell,
                clipboard_writes,
                progress,
                title,
                vt_modes,
                sync_output,
                capture,
                image_delta,
            )
        };

        if let Some(cwd) = pwd {
            self.event_proxy.send_event(TerminalEvent::Cwd(cwd));
        }

        // Ship new/changed kitty image pixels + removals via the existing graphics
        // event to the renderer's image store. Empty in steady state.
        let (pending_images, removed_ids) = image_delta;

        if !pending_images.is_empty() || !removed_ids.is_empty() {
            use crate::graphics::{GraphicId, UpdateQueues};

            self.event_proxy.send_event(TerminalEvent::UpdateGraphics {
                route_id: self.route_id,
                queues: UpdateQueues {
                    pending: Vec::new(),
                    pending_images,
                    remove_queue: removed_ids
                        .into_iter()
                        .map(|id| GraphicId(id as u64))
                        .collect(),
                },
            });
        }

        if bell > 0 {
            self.event_proxy.send_event(TerminalEvent::Bell);
        }

        for (ty, text) in clipboard_writes {
            self.event_proxy
                .send_event(TerminalEvent::ClipboardStore(ty, text));
        }

        if let Some(report) = progress {
            self.event_proxy
                .send_event(TerminalEvent::ProgressReport(report));
        }

        if let Some(title) = title {
            self.event_proxy.send_event(TerminalEvent::Title(title));
        }

        // Publish VT modes lock-free; this PTY task is the sole writer.
        self.vt_modes
            .store(vt_modes.bits(), sync::atomic::Ordering::Relaxed);

        if sync_output {
            self.sync_output_started_at.get_or_insert_with(Instant::now);
        } else {
            self.sync_output_started_at = None;
        }

        // Interactive-state detection: full-screen TUIs set alt-screen.
        self.prev_alt_screen = vt_modes.contains(vt_modes::Mode::ALT_SCREEN);

        self.emit_interactive_state();

        let Some(capture) = capture else {
            self.snapshot_pending = true;

            // The poll deadline retries a deferred capture without requiring
            // another byte or spinning on self-generated wakeups.
            return Ok(());
        };

        self.last_snapshot_at = Some(Instant::now());
        self.snapshot_pending = false;

        self.publish_capture(capture);

        Ok(())
    }

    /// Swap a finished capture into the shared frame store and announce the
    /// new frame. A failed capture leaves the previous frame published.
    fn publish_capture(&mut self, capture: ghostty::Result<()>) {
        #[cfg(enable_profiling)]
        let publish_started = self.profile.start();

        let published = publish_render_buffer(
            &self.render_buffer,
            &mut self.back_buffer,
            capture,
            self.ghostty.progress_active(),
            &mut self.capture_failed,
        );

        #[cfg(enable_profiling)]
        self.profile.record(Stage::Publish, publish_started);

        if published {
            self.event_proxy
                .send_event(TerminalEvent::TerminalDamaged(self.route_id));
        }
    }

    /// Tell the view this terminal closed.
    fn announce_closed(&self) {
        self.event_proxy
            .send_event(TerminalEvent::CloseTerminal(self.route_id));

        self.event_proxy.send_event(TerminalEvent::Render);
    }

    fn pending_snapshot_deadline(&self) -> Option<Instant> {
        if !self.snapshot_pending {
            return None;
        }

        Some(match self.sync_output_started_at {
            Some(started) => started + SYNC_OUTPUT_TIMEOUT,
            None => self
                .last_snapshot_at
                .map_or_else(Instant::now, |at| at + SNAPSHOT_MIN_INTERVAL),
        })
    }

    fn flush_pending_on_exit(&mut self) {
        if !self.snapshot_pending {
            return;
        }

        // No later timer or synchronized-update terminator will arrive after
        // the loop exits. Retain its final parsed state for closed-session views.
        if self.ghostty.mode(mode::SYNC_OUTPUT) {
            self.ghostty.write_vt(b"\x1b[?2026l");
        }

        if let Err(error) = self.flush_engine_state(FlushReason::Exit) {
            warn!("failed to publish final terminal frame: {error}");
        }
    }

    /// Collect a bounded batch and execute commands in submission order.
    fn drain_recv_channel(&mut self, state: &mut PtyState, cx: &mut Context<'_>) -> Step {
        let mut step = Step::Idle;

        for _ in 0..64 {
            let TaskPoll::Ready(Some(msg)) = self.receiver.poll_recv(cx) else {
                break;
            };

            step = Step::Advanced;

            // Only unexecuted, adjacent size changes are interchangeable.
            // An input or query between them observes the earlier geometry.
            match msg {
                Msg::Shutdown => return Step::Shutdown,
                Msg::PowerShellCompatibility(enabled) => {
                    self.powershell_compatibility.set_enabled(enabled);
                }
                Msg::Resize(size) => {
                    if let Some((Msg::Resize(previous), _)) = self.pending_commands.back_mut() {
                        *previous = size;
                    } else {
                        self.pending_commands.push_back((Msg::Resize(size), None));
                    }
                }
                Msg::Input(input) => self.pending_commands.push_back((
                    Msg::Input(input),
                    self.powershell_compatibility.input_limit(Instant::now()),
                )),
                request => self.pending_commands.push_back((request, None)),
            }
        }

        match self.process_pending_commands(state, cx) {
            Step::Idle => step,
            processed => processed,
        }
    }

    fn process_pending_commands(&mut self, state: &mut PtyState, cx: &mut Context<'_>) -> Step {
        let mut step = Step::Idle;

        for _ in 0..64 {
            if let Some(ResizeInFlight { size, grid_changed }) = self.pending_resize.clone() {
                match self.pty.poll_resize(cx, size) {
                    TaskPoll::Pending => return step,
                    TaskPoll::Ready(Err(error)) => warn!("PTY resize failed: {error}"),
                    TaskPoll::Ready(Ok(())) => {}
                }

                self.pending_resize = None;
                step = Step::Advanced;

                // The input pause starts when the native resize completes. An
                // alternate screen application owns its redraw, so no pause
                // applies there, and one left from an earlier resize ends.
                if self.ghostty.mode(mode::ALT_SCREEN) {
                    self.powershell_compatibility.clear_resize();
                } else if grid_changed {
                    self.powershell_compatibility.resized(Instant::now());
                }
            }

            if self
                .pending_input_deadline()
                .is_some_and(|deadline| deadline > Instant::now())
            {
                return step;
            }

            if matches!(self.pending_commands.front(), Some((Msg::Resize(_), _))) {
                if state.needs_write() {
                    return step;
                }

                match self.pty.poll_flush(cx) {
                    TaskPoll::Ready(Ok(())) => {}
                    TaskPoll::Pending => return step,
                    TaskPoll::Ready(Err(error)) => {
                        error!("failed to finish PTY input before resize: {error}");

                        return Step::Shutdown;
                    }
                }
            }

            let Some((msg, _)) = self.pending_commands.pop_front() else {
                return step;
            };

            step = Step::Advanced;

            match msg {
                Msg::Input(input) => {
                    state.write_list.push_back(input);
                }
                Msg::Resize(window_size) => {
                    let grid_changed = self.on_resize(window_size.clone());

                    self.pending_resize = Some(ResizeInFlight {
                        size: window_size,
                        grid_changed,
                    });
                }
                Msg::Shutdown => return Step::Shutdown,
                request => self.on_request(request),
            }
        }

        step
    }

    fn pending_input_deadline(&self) -> Option<Instant> {
        // Only completion can release a running native resize. An expired
        // input deadline must not keep waking a task still waiting for it.
        if self.pending_resize.is_some() {
            return None;
        }

        let (Msg::Input(bytes), limit) = self.pending_commands.front()? else {
            return None;
        };

        if bytes.is_empty() {
            return None;
        }

        let deadline = self.powershell_compatibility.deadline(*limit)?;

        (!self.ghostty.mode(mode::ALT_SCREEN)).then_some(deadline)
    }

    fn on_resize(&mut self, window_size: WinsizeBuilder) -> bool {
        // Keep the Ghostty engine sized to match the PTY/Crosswords.
        let cols = window_size.cols.max(1);
        let rows = window_size.rows.max(1);
        let grid_changed = self.ghostty.cols() != cols || self.ghostty.rows() != rows;
        let cell_w = (window_size.width / cols).max(1) as u32;
        let cell_h = (window_size.height / rows).max(1) as u32;

        let mut blocks_sync: Option<Vec<(ghostty::BlockHandle, usize)>> = None;

        let snapshot = {
            let engine = &mut self.ghostty;

            if vt_trace::enabled() {
                vt_trace::trace(
                    "perf_resize_before",
                    engine,
                    &format!(
                        "route={} request cols={} rows={} px={}x{} cell={}x{}",
                        self.route_id,
                        cols,
                        rows,
                        window_size.width,
                        window_size.height,
                        cell_w,
                        cell_h
                    ),
                );
            }

            if let Err(err) = engine.resize(cols, rows, cell_w, cell_h) {
                warn!("engine resize failed: {err:?}");
            }

            #[cfg(enable_profiling)]
            self.profile.set_grid(engine.cols(), engine.rows());

            if vt_trace::enabled() {
                vt_trace::trace(
                    "perf_resize_after_engine",
                    engine,
                    &format!(
                        "route={} applied cols={} rows={} cell={}x{}",
                        self.route_id, cols, rows, cell_w, cell_h
                    ),
                );
            }

            // Engine-blocks: resize eagerly reflowed every finished
            // block (new generations + row counts) — ship the fresh
            // list so the store's cached layout follows the engine reflow.
            if self.engine_blocks && engine.block_count() > 0 {
                blocks_sync = Some(engine_blocks_live_list(engine));
            }

            #[cfg(enable_profiling)]
            let capture_started = self.profile.start();

            self.content_version = self.content_version.wrapping_add(1);

            let capture = engine.snapshot_into(
                &mut self.back_buffer,
                self.content_version,
                self.theme_revision,
            );

            #[cfg(enable_profiling)]
            self.profile.record(Stage::CaptureResize, capture_started);

            capture
        };

        if let Some(live) = blocks_sync {
            self.event_proxy.send_event(TerminalEvent::BlockBatch(vec![
                event::BlockEvent::EngineBlocksSync(live),
            ]));
        }

        self.last_snapshot_at = Some(Instant::now());

        // VT modes do not change on resize, so the lock-free atomic remains
        // valid from the last PTY read.
        self.publish_capture(snapshot);

        grid_changed
    }

    #[inline]
    fn pty_write(&mut self, state: &mut PtyState, cx: &mut Context<'_>) -> io::Result<usize> {
        state.write_to(&mut self.pty, cx)
    }

    pub(crate) fn channel(&self) -> MsgSender {
        self.sender.clone()
    }

    async fn run_event_loop(mut self) -> (Self, PtyState) {
        let mut state = PtyState::default();
        let mut buf = vec![0u8; READ_BUFFER_SIZE];
        let mut io_failed = false;

        // Sustained output keeps a capture deadline pending on every batch.
        // Reusing one registered timer and moving it only when the deadline
        // changes avoids a timer-wheel insert and removal per batch.
        let mut timer = pin!(sleep_until(Instant::now()));

        loop {
            #[cfg(enable_profiling)]
            self.profile.report_due();

            let deadline = self
                .pending_snapshot_deadline()
                .into_iter()
                .chain(self.pending_input_deadline())
                .min();

            if let Some(deadline) = deadline
                && timer.deadline() != deadline
            {
                timer.as_mut().reset(deadline);
            }

            let result = tokio::select! {
                result = poll_fn(|cx| self.poll_io(&mut state, &mut buf, cx)) => result,
                () = &mut timer, if deadline.is_some() => {
                    if self.snapshot_pending {
                        self.flush_engine_state(FlushReason::Drained).map(|()| true)
                    } else {
                        Ok(true)
                    }
                }
            };

            match result {
                Ok(true) => {}
                Ok(false) => break,
                Err(error) => {
                    if error.kind() != ErrorKind::BrokenPipe {
                        error!("PTY I/O failed: {error}");
                    }

                    io_failed = true;

                    break;
                }
            }

            // Ready pipes can stay ready indefinitely. Bound each parse/write
            // batch and yield so other terminals and network tasks can run.
            yield_now().await;
        }

        // Publish the final state before console teardown, which can wait for
        // the shell tree, so a closed session is reported without that delay.
        self.flush_pending_on_exit();

        if self.child_exited || io_failed {
            self.announce_closed();
        }

        // A native resize owns the console while it runs, and the console
        // close needs the host to finish its writes. Keep draining output so a
        // full pipe cannot stall either one.
        if let Err(error) = poll_fn(|cx| {
            let completion = self.pty.poll_shutdown(cx);

            if completion.is_pending()
                && self
                    .pty_read(&mut state, &mut buf, cx)
                    .is_ok_and(|read| read != 0)
            {
                cx.waker().wake_by_ref();
            }

            completion
        })
        .await
        {
            warn!("failed to finish PTY control work: {error}");
        }

        #[cfg(enable_profiling)]
        self.profile.flush();

        (self, state)
    }

    fn poll_io(
        &mut self,
        state: &mut PtyState,
        buf: &mut [u8],
        cx: &mut Context<'_>,
    ) -> TaskPoll<io::Result<bool>> {
        let mut advanced = false;

        match self.drain_recv_channel(state, cx) {
            Step::Shutdown => return TaskPoll::Ready(Ok(false)),
            Step::Advanced => advanced = true,
            Step::Idle => {}
        }

        let read = self.pty_read(state, buf, cx)?;
        let written = self.pty_write(state, cx)?;

        match self.process_pending_commands(state, cx) {
            Step::Shutdown => return TaskPoll::Ready(Ok(false)),
            Step::Advanced => advanced = true,
            Step::Idle => {}
        }

        if !self.child_exited {
            self.child_exited = self.pty.poll_exit(cx).is_ready();
        }

        // A child can exit with several parse batches still buffered. Keep
        // yielding between them before publishing the final closed state.
        if self.child_exited && read < MAX_READ_BATCH {
            return TaskPoll::Ready(Ok(false));
        }

        // Progress means another pass may find more; parking relies on every
        // pending source having registered the task's waker.
        if advanced || read != 0 || written != 0 {
            TaskPoll::Ready(Ok(true))
        } else {
            TaskPoll::Pending
        }
    }

    fn on_request(&mut self, request: Msg) {
        match request {
            Msg::Scroll(delta) => {
                self.ghostty.scroll_viewport_delta(delta);

                self.publish_command();
            }
            Msg::ScrollTo(target) => {
                let scrollbar = self.ghostty.scrollbar();
                let target = target.min(scrollbar.total.saturating_sub(scrollbar.len));
                let target: i128 = target.into();
                let offset: i128 = scrollbar.offset.into();

                let delta =
                    (target - offset).clamp(isize::MIN as i128, isize::MAX as i128) as isize;

                self.ghostty.scroll_viewport_delta(delta);

                self.publish_command();
            }
            Msg::ScrollToEnd => {
                self.ghostty.scroll_viewport_bottom();

                self.publish_command();
            }
            Msg::Theme(colors) => {
                self.ghostty.set_theme_colors(&colors);

                self.theme_revision = self.theme_revision.wrapping_add(1);

                self.publish_command();
            }
            Msg::CursorShape { shape, reply } => {
                let result = self
                    .ghostty
                    .set_default_cursor_shape(shape)
                    .map_err(|error| RequestError::Engine(error.to_string()));

                if result.is_ok() {
                    self.publish_command();
                }

                let _ = reply.send(result);
            }
            Msg::Query(query) => {
                #[cfg(enable_profiling)]
                let query_started = self.profile.start();

                answer_query(
                    &mut self.ghostty,
                    self.content_version,
                    self.theme_revision,
                    query,
                );

                #[cfg(enable_profiling)]
                self.profile.record(Stage::Query, query_started);

                self.event_proxy.send_event(TerminalEvent::ReadReady);
            }
            Msg::Checkpoint(request) => {
                #[cfg(enable_profiling)]
                let checkpoint_started = self.profile.start();

                let result = self
                    .ghostty
                    .format_vt_state()
                    .map(|vt| Checkpoint {
                        vt,
                        cols: self.ghostty.cols(),
                        rows: self.ghostty.rows(),
                    })
                    .map_err(|error| RequestError::Engine(error.to_string()));

                (request.0)(result);

                #[cfg(enable_profiling)]
                self.profile.record(Stage::Checkpoint, checkpoint_started);
            }
            Msg::Input(_) | Msg::Resize(_) | Msg::Shutdown | Msg::PowerShellCompatibility(_) => {
                unreachable!("handled by the PTY loop")
            }
        }
    }

    fn publish_command(&mut self) {
        self.content_version = self.content_version.wrapping_add(1);
        self.snapshot_pending = true;

        #[cfg(enable_profiling)]
        let flush_started = {
            let started = self.profile.start();

            self.profile.command();

            started
        };

        let result = self.flush_engine_state(FlushReason::Command);

        #[cfg(enable_profiling)]
        self.profile.record(Stage::Flush, flush_started);

        if let Err(error) = result {
            warn!("failed to publish terminal update: {error}");
        }
    }
}
