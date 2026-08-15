use std::io::{self, ErrorKind, Read, Write};
use std::sync::atomic::{AtomicU32, AtomicU64};
use std::sync::{self, Arc, mpsc};
use std::thread::{Builder, JoinHandle};
use std::{cell, error, fmt, mem, path, time};

#[cfg(target_os = "linux")]
use libc::EIO;
use nmt_config::colors::Colors;
use nmt_platform::{ChildEvent, EventedPty, Events, Interest, Poll, Token, Waker};
use parking_lot::FairMutex;
use tracing::{error, warn};

use crate::event::{self, EventListener, Msg, MsgSender, TerminalEvent, WindowId};
use crate::ghostty::{self, GhosttyTerminal, mode};
use crate::prompt_sniffer::PromptSniffer;
use crate::render_buffer::RenderBuffer;
use crate::{terminal, vt_trace};

mod conpty_realign;
mod marks;
mod session;
mod write_queue;

#[cfg(test)]
use crate::pty_pipe::conpty_realign::max_cup_row_col;
use crate::pty_pipe::conpty_realign::{
    is_conpty_resize_echo_input, is_conpty_resize_repaint, rewrite_conpty_resize_echo_cup_rows,
    su_realign_count,
};
use crate::pty_pipe::marks::{apply_sniffer_mark, engine_blocks_live_list};
pub use crate::pty_pipe::session::{OutputSink, SessionHandles, SessionOptions, start_session};
pub use crate::pty_pipe::write_queue::PtyState;

/// Reserved `Poll` token for the loop's `Waker`. PTY source tokens start above it.
const WAKER_TOKEN: Token = Token(0);

const READ_BUFFER_SIZE: usize = 0x10_0000;
/// Max bytes to read from the PTY while the terminal is locked.
const MAX_LOCKED_READ: usize = u16::MAX as usize;
/// Min interval between full-viewport `snapshot()` + render-buffer readbacks
/// while PTY input is saturated (a batch hit `MAX_LOCKED_READ` with more data
/// pending). The readback is a constant-cost per-cell FFI walk (~1ms at 220×55)
/// that used to run once per batch and dominated the vtebench cell benchmarks.
/// The renderer samples
/// the render buffer at most once per display frame, so under saturation one
/// readback per frame is enough. 5ms sits below one 144 Hz frame (6.94ms), so
/// high-refresh displays still get a fresh grid every frame; plumb the real
/// monitor refresh rate here if 240 Hz+ becomes a target. A caught-up read
/// (interactive echo) always snapshots — this only coalesces under saturation.
const SNAPSHOT_MIN_INTERVAL: time::Duration = time::Duration::from_millis(5);
/// Match Windows Terminal's upper bound so a missing DEC 2026 reset cannot
/// leave the last committed frame visible indefinitely.
const SYNC_OUTPUT_TIMEOUT: time::Duration = time::Duration::from_millis(100);

/// Escape raw PTY bytes for human-readable vt_trace output.
fn escape_bytes(bytes: &[u8]) -> String {
    let mut out = String::new();

    for &b in bytes {
        match b {
            b'\x1b' => out.push_str("<ESC>"),
            b'\r' => out.push_str("<CR>"),
            b'\n' => out.push_str("<LF>"),
            0x20..=0x7e => out.push(b as char),
            _ => {
                let _ = fmt::Write::write_fmt(&mut out, format_args!("\\x{b:02x}"));
            }
        }
    }

    out
}

pub struct PtyPipe<T: EventedPty, U: EventListener> {
    sender: MsgSender,
    receiver: mpsc::Receiver<Msg>,
    pty: T,
    poll: Poll,
    /// The loop's `Waker`. On Windows the ConPTY worker threads and the child-exit
    /// callback signal readiness through it; the `MsgSender` wakes it after each send
    /// (mio 1.2 has no pollable channel / user-space readiness).
    waker: Arc<Waker>,
    /// Ghostty VT engine driving the terminal state. Shared behind a lock so the
    /// frontend can reach it synchronously for scroll, selection, and search. The
    /// PTY thread locks it for `write_vt` and render-state reads; the
    /// render buffer is on a separate lock so a frame never waits behind a parse.
    ghostty: Arc<FairMutex<GhosttyTerminal>>,
    /// Ghostty-fed viewport copy the renderer reads. Populated here
    /// each batch; on its own lock, separate from the engine so paint never
    /// waits behind parsing.
    render_buffer: Arc<FairMutex<RenderBuffer>>,
    /// PTY-thread-private target for direct Ghostty capture. A completed frame
    /// swaps with `render_buffer`, so the shared lock covers only publication.
    back_buffer: RenderBuffer,
    /// VT modes published to the frontend. This `PtyPipe` is the sole writer;
    /// the input path reads it lock-free. `Mode` is `u32`.
    vt_modes: Arc<AtomicU32>,
    /// Monotonic content version, bumped once per PTY batch / resize (the engine
    /// exposes no generation signal). The frontend's deep-search corpus cache
    /// compares it to detect content changes and invalidate cached views.
    content_version: Arc<AtomicU64>,
    /// Optional observer for the exact VT stream accepted by the engine. It runs
    /// under the engine lock so a checkpoint and its following byte sequence can
    /// be ordered atomically; observers must therefore return promptly.
    output_sink: Option<Arc<dyn Fn(Arc<[u8]>) + Send + Sync>>,
    terminal_responses_enabled: bool,
    event_proxy: U,
    window_id: WindowId,
    route_id: usize,
    conpty_resize_echo_realign: bool,
    conpty_resize_echo_pending: bool,
    conpty_resize_repaint_reads_remaining: u8,
    /// When the last resize happened. The `conpty_resize_echo_realign` input
    /// gate only fires for a brief window after a resize — input typed *during*
    /// the resize repaint, whose ConPTY echo lands at a stale CUP. Without a time
    /// bound, ordinary typing long after a resize keeps tripping the gate and
    /// accumulates (the "type 120 x's after a resize and the prompt repeats" bug).
    conpty_resize_at: Option<time::Instant>,
    /// SU-realign latch. `active_cursor_row()` is captured at resize time
    /// (it flips within one frame during the ConPTY repaint storm, so it can't be read
    /// live). `*_cols/_rows` let the first repaint sanity-check it answers this resize.
    conpty_resize_prompt_row: u16,
    conpty_resize_cols: u16,
    conpty_resize_rows: u16,
    su_realign_armed: bool,
    /// OSC 133 region state; only touched on the PTY thread.
    sniffer: PromptSniffer,
    /// Launch cwd of the currently-running command, latched at its `;C`: the cwd MUST
    /// NOT be read at `;D`, by which point the ps1 has already reported the NEXT
    /// prompt's OSC 7 directory (which would mislabel every `cd`). Attached to the
    /// CommandFinished event. PTY-thread-private.
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
    /// Last InteractiveState value sent, so the signal stays edge-triggered.
    prev_interactive: bool,
    /// Last AltScreen value sent.
    prev_alt_screen_sent: bool,
    /// When the last `snapshot()` readback ran, for saturation coalescing
    /// (`SNAPSHOT_MIN_INTERVAL`). PTY-thread-private.
    last_snapshot_at: time::Instant,
    /// True when a saturated batch or synchronized update deferred its readback.
    /// Makes the event loop run `pty_read` without requiring new PTY bytes.
    snapshot_pending: bool,
    /// Start of the current DEC 2026 transaction. The event-loop poll uses this
    /// deadline to recover when an application omits the matching reset.
    sync_output_started_at: Option<time::Instant>,
}

/// Read the VT-controlled modes from the Ghostty engine into `Mode`.
/// bits (e.g. `Mode::VI`) are not touched here — see
/// [`Crosswords::sync_vt_modes`].
fn ghostty_vt_modes(g: &GhosttyTerminal) -> terminal::Mode {
    use crate::ghostty::mode as gm;
    use crate::terminal::Mode;

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
    front: &FairMutex<RenderBuffer>,
    back: &mut RenderBuffer,
    capture: ghostty::Result<()>,
    hide_cursor: bool,
) -> bool {
    if capture.is_err() {
        return false;
    }

    if hide_cursor {
        back.set_cursor_visible(false);
    }

    mem::swap(&mut *front.lock(), back);

    true
}

/// Convert an OSC 7 working-directory value to a path. Strips a `file://host`
/// prefix when present (`file://host/path` → `/path`); otherwise uses the
/// value verbatim.
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

impl<T, U> PtyPipe<T, U>
where
    T: EventedPty + Send + 'static,
    U: EventListener + Send + 'static,
{
    pub(crate) fn new(
        render_buffer: Arc<FairMutex<RenderBuffer>>,
        vt_modes: Arc<AtomicU32>,
        pty: T,
        event_proxy: U,
        window_id: WindowId,
        route_id: usize,
        colors: Colors,
        scrollback_lines: usize,
        engine_blocks: bool,
    ) -> Result<PtyPipe<T, U>, Box<dyn error::Error>> {
        let poll = Poll::new()?;

        // The `Waker` is registered on a reserved token; the worker threads (Windows)
        // and the `MsgSender` wake the loop through it.
        let waker = Arc::new(Waker::new(poll.registry(), WAKER_TOKEN)?);
        let (tx, rx) = mpsc::channel();
        let sender = MsgSender::new(tx, waker.clone());

        // Start the engine at the render buffer's viewport dimensions so the first
        // resize cannot diverge from a zero-sized construction.
        let (cols, rows) = {
            let rb = render_buffer.lock();
            (rb.cols() as u16, rb.rows() as u16)
        };

        // `max_scrollback` is a **byte budget** in the engine — the C-binding's
        // "lines" doc is wrong (verified: budgets ≤1 MB floor at ~3297 lines, 10 MB
        // holds ~36k at 20 cols ≈ 273 B/line). `scrollback-history-limit` is
        // in lines, so convert through `scrollback_bytes`.
        let max_scrollback = scrollback_bytes(scrollback_lines, cols.max(1));

        let mut ghostty = GhosttyTerminal::new(cols.max(1), rows.max(1), max_scrollback)
            .map_err(|err| Box::new(err) as Box<dyn error::Error>)?;

        // Push the host theme's default colors + 256-palette into the engine so
        // SGR-indexed and default colors resolve to theme.
        ghostty.set_theme_colors(&colors);

        // Seed the atomic with the engine's initial VT modes (SHOW_CURSOR,
        // LINE_WRAP, …) so the facade is correct before the first PTY batch.
        vt_modes.store(
            ghostty_vt_modes(&ghostty).bits(),
            sync::atomic::Ordering::Relaxed,
        );

        Ok(PtyPipe {
            sender,
            receiver: rx,
            poll,
            waker,
            pty,
            ghostty: Arc::new(FairMutex::new(ghostty)),
            render_buffer,
            back_buffer: RenderBuffer::new(cols as usize, rows as usize),
            vt_modes,
            content_version: Arc::new(AtomicU64::new(0)),
            output_sink: None,
            terminal_responses_enabled: true,
            event_proxy,
            window_id,
            route_id,
            conpty_resize_echo_realign: false,
            conpty_resize_echo_pending: false,
            conpty_resize_repaint_reads_remaining: 0,
            conpty_resize_at: None,
            conpty_resize_prompt_row: 0,
            conpty_resize_cols: 0,
            conpty_resize_rows: 0,
            su_realign_armed: false,
            sniffer: PromptSniffer::default(),
            launch_cwd: None,
            engine_blocks,
            mark_seq: 0,
            prev_alt_screen: false,
            prev_interactive: false,
            prev_alt_screen_sent: false,
            last_snapshot_at: time::Instant::now(),
            snapshot_pending: false,
            sync_output_started_at: None,
        })
    }

    /// Emit alt-screen interactive state, edge-triggered so a stable state emits nothing.
    fn emit_interactive_state(&mut self) {
        let on = self.prev_alt_screen;

        if on != self.prev_interactive {
            self.prev_interactive = on;
            self.event_proxy
                .send_event(TerminalEvent::InteractiveState(on), self.window_id);
        }

        if self.prev_alt_screen != self.prev_alt_screen_sent {
            self.prev_alt_screen_sent = self.prev_alt_screen;

            self.event_proxy.send_event(
                TerminalEvent::AltScreen(self.prev_alt_screen),
                self.window_id,
            );
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
    fn pty_read(&mut self, _state: &mut PtyState, buf: &mut [u8]) -> io::Result<()> {
        let mut unprocessed = 0;
        let mut processed = 0;

        // True when the loop drained the PTY (WouldBlock/EOF); false when it
        // broke at MAX_LOCKED_READ with more data likely pending.
        let mut caught_up = false;

        loop {
            // Read from the PTY.
            match self.pty.reader().read(&mut buf[unprocessed..]) {
                // This is received on Windows/macOS when no more data is readable from the PTY.
                Ok(0) if unprocessed == 0 => {
                    caught_up = true;
                    break;
                }
                Ok(got) => unprocessed += got,
                Err(err) => match err.kind() {
                    ErrorKind::Interrupted | ErrorKind::WouldBlock => {
                        // Go back to mio if we're caught up on parsing and the PTY would block.
                        if unprocessed == 0 {
                            caught_up = true;
                            break;
                        }
                    }
                    _ => return Err(err),
                },
            }

            self.process_pty_chunk(&buf[..unprocessed]);

            // Content changed, so invalidate the cached deep-search corpus.
            self.content_version
                .fetch_add(1, sync::atomic::Ordering::Relaxed);

            processed += unprocessed;
            unprocessed = 0;

            // Don't accumulate unboundedly before reflecting to the renderer.
            if processed >= MAX_LOCKED_READ {
                break;
            }
        }

        if processed == 0 && !self.snapshot_pending {
            return Ok(());
        }

        self.flush_engine_state(caught_up)
    }

    #[inline]
    fn process_pty_chunk(&mut self, input: &[u8]) {
        let input_len = input.len();

        // Feed the bytes into Ghostty's VT engine (under the engine lock).
        // The render buffer is on a separate lock, so this never blocks a
        // render frame while the same engine snapshot is locked.
        let output_sink = self.output_sink.clone();

        let mut engine = self.ghostty.lock();

        let echo_pending_at_entry = cfg!(windows) && self.conpty_resize_echo_pending;

        let mut rewritten = None;
        let mut synthetic_prefix = None;

        // Some(R_conpty) means SU realignment ran during this read.
        let mut su_realigned_to: Option<u16> = None;

        let repaint_window = cfg!(windows) && self.conpty_resize_repaint_reads_remaining > 0;

        if repaint_window {
            self.conpty_resize_repaint_reads_remaining =
                self.conpty_resize_repaint_reads_remaining.saturating_sub(1);
        }

        // SU realignment runs before and instead of the legacy CUP rewrite.
        // On the first repaint of a resize, push ghostty's active-top history rows
        // into scrollback so its prompt rises to ConPTY's row; then ConPTY's own
        // CUP in the ORIGINAL repaint lands on the prompt (no 0005 rewrite). The
        // `su_realign_count` pre-check (latched size correspondence + N>0) and the
        // post-write assertion below guard against a mis-latched resize.
        if cfg!(windows) && self.su_realign_armed && repaint_window {
            self.su_realign_armed = false;

            if !engine.mode(mode::ALT_SCREEN) {
                if let Some((n, r_conpty)) = su_realign_count(
                    input,
                    self.conpty_resize_prompt_row,
                    self.conpty_resize_cols,
                    self.conpty_resize_rows,
                    engine.cols(),
                    engine.rows(),
                ) {
                    let realign = format!("\x1b[{n}S").into_bytes();

                    engine.write_vt(&realign);

                    synthetic_prefix = Some(realign);

                    su_realigned_to = Some(r_conpty);

                    self.conpty_resize_echo_pending = false;
                }
            }
        }

        if cfg!(windows)
            && su_realigned_to.is_none()
            && (self.conpty_resize_echo_pending || repaint_window)
        {
            if engine.mode(mode::ALT_SCREEN) {
                self.conpty_resize_echo_pending = false;
                self.conpty_resize_repaint_reads_remaining = 0;
            } else if let Some(active_row) = engine.active_cursor_row() {
                // Realign ConPTY's resize echo/repaint to the engine's ACTIVE
                // cursor row. `active_cursor_row()` reads the active-screen
                // cursor (the frame CUP addresses) straight from the engine, so
                // it stays correct regardless of viewport scroll or blank rows
                // below the prompt — unlike the render-state
                // `snapshot.cursor.y`, which is viewport-relative and reads as
                // row 0 when scrolled (forcing the echo+erase onto a visible
                // history row). With a reliable target there's no need for the
                // old at-bottom gate or the PTY-thread snap-to-bottom; the echo
                // always routes to the true prompt row (the frontend still
                // snaps the *view* to the bottom on keypress for UX).
                let target_row = active_row.saturating_add(1);

                let repaint_pending = repaint_window && is_conpty_resize_repaint(input, target_row);

                if self.conpty_resize_echo_pending || repaint_pending {
                    rewritten = rewrite_conpty_resize_echo_cup_rows(input, target_row);
                }

                if rewritten.is_some() || repaint_pending {
                    self.conpty_resize_echo_pending = false;

                    if repaint_pending {
                        self.conpty_resize_repaint_reads_remaining = 0;
                    }
                }
            }
        }

        let bytes = rewritten.as_deref().unwrap_or(input);

        let observed_output = output_sink.as_ref().map(|_| match synthetic_prefix {
            Some(mut prefix) => {
                prefix.extend_from_slice(bytes);

                Arc::from(prefix)
            }
            None => Arc::from(bytes),
        });

        let was_rewritten = rewritten.is_some();

        // Capture the pre-rewrite ConPTY bytes + cursor visibility for the
        // trace so we can compare original vs rewritten CUP rows.
        let (orig_escaped, cursor_vis_at_decision) =
            if vt_trace::enabled() && (repaint_window || was_rewritten || echo_pending_at_entry) {
                let orig = input;
                (
                    escape_bytes(&orig[..orig.len().min(240)]),
                    engine.snapshot().ok().map(|s| s.cursor_visible()),
                )
            } else {
                (String::new(), None)
            };

        // Always run the sniffer: it classifies/captures OSC 133 lifecycle state
        // while every byte, including marks, still reaches the engine.

        // Both feed closures need the engine (segment/mark forwarding
        // writes; cwd latch reads) and never run at the same time, so a
        // RefCell shares the one borrow.
        let engine_cell = cell::RefCell::new(&mut *engine);
        let launch_cwd = &mut self.launch_cwd;
        let mark_seq = &mut self.mark_seq;
        let event_proxy = &self.event_proxy;
        let window_id = self.window_id;
        let engine_blocks = self.engine_blocks;

        self.sniffer.feed_hooked(
            bytes,
            |_, _, seg| {
                engine_cell.borrow_mut().write_vt(seg);
            },
            |mark| {
                apply_sniffer_mark(
                    &engine_cell,
                    launch_cwd,
                    mark_seq,
                    event_proxy,
                    window_id,
                    engine_blocks,
                    mark,
                );
            },
        );

        if let Some(trusted) = self.sniffer.take_boundary_trust_changed() {
            self.event_proxy.send_event(
                TerminalEvent::PromptBoundaryTrusted(trusted),
                self.window_id,
            );
        }

        // No steady-state work per read: the whole command is
        // frozen once at its `;D` finish (the per-line harvest
        // this replaced was the throughput cost the per-block
        // grid exists to delete).

        // After SU realignment and the original repaint, ConPTY's own
        // absolute CUP must have pulled the active cursor onto the realigned
        // prompt row. A mismatch means the realign assumption broke (mis-latched
        // resize, divergent wrap) — trace + debug_assert, never a prod panic.
        if let Some(expected) = su_realigned_to {
            let got = engine.active_cursor_row();

            if got != Some(expected) && vt_trace::enabled() {
                vt_trace::trace(
                    "su_realign_assert",
                    &mut engine,
                    &format!("expected R_conpty={expected} got={got:?}"),
                );
            }

            debug_assert_eq!(
                got,
                Some(expected),
                "SU realign: active cursor should land on R_conpty"
            );
        }

        // Dump the full VT only for resize-related reads (the ConPTY repaint
        // window or a realigned echo) — a per-keystroke full dump would be
        // O(scrollback) on every read.
        if vt_trace::enabled() && (repaint_window || was_rewritten || echo_pending_at_entry) {
            vt_trace::trace(
                "pty_resize_read",
                &mut engine,
                &format!(
                    "read_bytes={input_len} rewritten={was_rewritten} \
                         repaint_window={repaint_window} echo_pending={echo_pending_at_entry} \
                         cursor_vis_pre={cursor_vis_at_decision:?} \
                         orig={orig_escaped} \
                         bytes={}",
                    escape_bytes(&bytes[..bytes.len().min(240)])
                ),
            );
        }

        if let (Some(sink), Some(output)) = (&output_sink, observed_output) {
            sink(output);
        }
    }

    #[inline]
    fn flush_engine_state(&mut self, caught_up: bool) -> io::Result<()> {
        // Coalesce the full-viewport readback: a caught-up read always
        // snapshots (interactive echo and the stream's final state land with no
        // added latency); a saturated read rate-limits it to
        // SNAPSHOT_MIN_INTERVAL so the readback stops dominating parse
        // throughput (see the constant's doc).
        let do_snapshot = caught_up || self.last_snapshot_at.elapsed() >= SNAPSHOT_MIN_INTERVAL;

        // Drain everything from the engine under ONE lock: query/DSR/DA
        // responses, bell, title, pwd, VT modes, and the snapshot. Then act on
        // the owned results below with the engine lock released.
        let (responses, bell, clipboard_writes, title, vt_modes, sync_output, capture, image_delta) = {
            let mut engine = self.ghostty.lock();

            let responses = engine.take_pty_writes();
            let bell = engine.take_bell();
            let clipboard_writes = engine.take_clipboard_writes();
            let title = engine.poll_title();
            let vt_modes = ghostty_vt_modes(&engine);
            let sync_output_timed_out = self
                .sync_output_started_at
                .is_some_and(|started| started.elapsed() >= SYNC_OUTPUT_TIMEOUT);

            if sync_output_timed_out && engine.mode(mode::SYNC_OUTPUT) {
                engine.write_vt(b"\x1b[?2026l");
            }

            let sync_output = engine.mode(mode::SYNC_OUTPUT);

            let (capture, image_delta) = if do_snapshot && !sync_output {
                let capture = engine.snapshot_into(&mut self.back_buffer);

                // Kitty image pixel deltas, under the same lock. Only the PTY
                // reader path drives image shipping; the scroll path never calls this.
                let image_delta = match &capture {
                    Ok(()) => engine.take_image_deltas(self.back_buffer.placements()),
                    Err(_) => (Vec::new(), Vec::new()),
                };
                (Some(capture), image_delta)
            } else {
                (None, (Vec::new(), Vec::new()))
            };

            (
                responses,
                bell,
                clipboard_writes,
                title,
                vt_modes,
                sync_output,
                capture,
                image_delta,
            )
        };

        // Ship new/changed kitty image pixels + removals via the existing graphics
        // event to the renderer's image store. Empty in steady state.
        let (pending_images, removed_ids) = image_delta;

        if !pending_images.is_empty() || !removed_ids.is_empty() {
            use crate::graphics::{GraphicId, UpdateQueues};

            self.event_proxy.send_event(
                TerminalEvent::UpdateGraphics {
                    route_id: self.route_id,
                    queues: UpdateQueues {
                        pending: Vec::new(),
                        pending_images,
                        remove_queue: removed_ids
                            .into_iter()
                            .map(|id| GraphicId(id as u64))
                            .collect(),
                    },
                },
                self.window_id,
            );
        }

        if self.terminal_responses_enabled && !responses.is_empty() {
            let _ = self.pty.writer().write_all(&responses);
        }

        if bell > 0 {
            self.event_proxy
                .send_event(TerminalEvent::Bell, self.window_id);
        }

        for (ty, text) in clipboard_writes {
            self.event_proxy
                .send_event(TerminalEvent::ClipboardStore(ty, text), self.window_id);
        }

        if let Some(title) = title {
            self.event_proxy
                .send_event(TerminalEvent::Title(title), self.window_id);
        }

        // Publish VT modes lock-free; this PTY thread is the sole writer.
        self.vt_modes
            .store(vt_modes.bits(), sync::atomic::Ordering::Relaxed);

        if sync_output {
            self.sync_output_started_at
                .get_or_insert_with(time::Instant::now);
        } else {
            self.sync_output_started_at = None;
        }

        // Interactive-state detection: full-screen TUIs set alt-screen.
        self.prev_alt_screen = vt_modes.contains(terminal::Mode::ALT_SCREEN);

        self.emit_interactive_state();

        // Readback skipped under saturation: self-wake so another `pty_read`
        // pass runs even if the pipe drained exactly at the MAX_LOCKED_READ
        // boundary (no OS readiness would re-fire, and the Windows soft-ready
        // flag may already be clear). That pass either parses more pending data
        // or reads 0 bytes, lands caught-up, and flushes this pending snapshot.
        let Some(capture) = capture else {
            self.snapshot_pending = true;

            // A synchronized frame needs more PTY bytes (normally DEC reset 2026)
            // before it is safe to publish. Waking immediately would spin while the
            // pipe is empty; the next readable event will commit the complete frame.
            if !sync_output {
                let _ = self.waker.wake();
            }

            return Ok(());
        };

        self.last_snapshot_at = time::Instant::now();
        self.snapshot_pending = false;

        if publish_render_buffer(
            &self.render_buffer,
            &mut self.back_buffer,
            capture,
            self.sniffer.progress_active(),
        ) {
            self.event_proxy.send_event(
                TerminalEvent::TerminalDamaged(self.route_id),
                self.window_id,
            );
        }

        Ok(())
    }

    /// Drain the channel.
    ///
    /// Returns `false` when a shutdown message was received.
    fn drain_recv_channel(&mut self, state: &mut PtyState) -> bool {
        while let Ok(msg) = self.receiver.try_recv() {
            match msg {
                Msg::Input(input) => {
                    // Only treat input as a resize echo for a brief window after a
                    // resize (the keystroke typed *during* the repaint, whose ConPTY
                    // echo lands at a stale CUP). The `reads_remaining` countdown
                    // doesn't close this window when the user is idle then types a
                    // fast burst (one channel drain → one `pty_read`), so bound it by
                    // time. Without this, ordinary typing after a resize keeps being
                    // realigned and the input/prompt accumulates.
                    const RESIZE_ECHO_WINDOW: time::Duration = time::Duration::from_millis(150);

                    if cfg!(windows)
                        && self.conpty_resize_echo_realign
                        && self
                            .conpty_resize_at
                            .is_some_and(|t| t.elapsed() < RESIZE_ECHO_WINDOW)
                        && is_conpty_resize_echo_input(input.as_ref())
                    {
                        self.conpty_resize_echo_pending = true;
                    }

                    state.write_list.push_back(input)
                }
                Msg::Resize(window_size) => {
                    // Keep the Ghostty engine sized to match the PTY/Crosswords.
                    let cols = window_size.cols.max(1);
                    let rows = window_size.rows.max(1);
                    let cell_w = (window_size.width / cols).max(1) as u32;
                    let cell_h = (window_size.height / rows).max(1) as u32;
                    let mut blocks_sync: Option<Vec<(ghostty::BlockHandle, usize)>> = None;

                    let (snapshot, active_row) = {
                        let mut engine = self.ghostty.lock();

                        if vt_trace::enabled() {
                            vt_trace::trace(
                                "perf_resize_before",
                                &mut engine,
                                &format!(
                                    "request cols={} rows={} px={}x{} cell={}x{}",
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

                        if vt_trace::enabled() {
                            vt_trace::trace(
                                "perf_resize_after_engine",
                                &mut engine,
                                &format!(
                                    "applied cols={} rows={} cell={}x{}",
                                    cols, rows, cell_w, cell_h
                                ),
                            );
                        }

                        // Engine-blocks: resize eagerly reflowed every finished
                        // block (new generations + row counts) — ship the fresh
                        // list so the store's cached layout follows the engine reflow.
                        if self.engine_blocks && engine.block_count() > 0 {
                            blocks_sync = Some(engine_blocks_live_list(&engine));
                        }

                        let capture = engine.snapshot_into(&mut self.back_buffer);

                        (capture, engine.active_cursor_row())
                    };

                    if let Some(live) = blocks_sync {
                        self.event_proxy.send_event(
                            TerminalEvent::BlockBatch(vec![event::BlockEvent::EngineBlocksSync(
                                live,
                            )]),
                            self.window_id,
                        );
                    }

                    if publish_render_buffer(
                        &self.render_buffer,
                        &mut self.back_buffer,
                        snapshot,
                        self.sniffer.progress_active(),
                    ) {
                        // VT modes do not change on resize, so the lock-free
                        // atomic remains valid from the last PTY read.
                        self.event_proxy.send_event(
                            TerminalEvent::TerminalDamaged(self.route_id),
                            self.window_id,
                        );
                    }

                    // Resize reflows content → invalidate any deep-search corpus.
                    self.content_version
                        .fetch_add(1, sync::atomic::Ordering::Relaxed);

                    if cfg!(windows) {
                        self.conpty_resize_echo_realign = true;
                        self.conpty_resize_at = Some(time::Instant::now());
                        self.conpty_resize_repaint_reads_remaining = 8;

                        // Latch the prompt row and size now so SU realignment uses the
                        // pre-resize cursor position.
                        // `active_cursor_row()` is on the prompt row here, but flips
                        // within one frame once ConPTY's first repaint CUP lands, so it
                        // must be captured at resize time, not read live in the storm.
                        self.conpty_resize_prompt_row = active_row.unwrap_or(0);
                        self.conpty_resize_cols = cols;
                        self.conpty_resize_rows = rows;
                        self.su_realign_armed = active_row.is_some();
                    }

                    if let Err(err) = self.pty.set_winsize(window_size) {
                        warn!("pty set_winsize failed: {err}");
                    }
                }
                Msg::Shutdown => return false,
            }
        }

        true
    }

    #[inline]
    fn pty_write(&mut self, state: &mut PtyState) -> io::Result<()> {
        state.ensure_next();

        'write_many: while let Some(mut current) = state.take_current() {
            'write_one: loop {
                match self.pty.writer().write(current.remaining_bytes()) {
                    Ok(0) => {
                        state.set_current(Some(current));

                        break 'write_many;
                    }
                    Ok(n) => {
                        current.advance(n);

                        if current.finished() {
                            state.goto_next();

                            break 'write_one;
                        }
                    }
                    Err(err) => {
                        state.set_current(Some(current));

                        match err.kind() {
                            ErrorKind::Interrupted | ErrorKind::WouldBlock => break 'write_many,
                            _ => return Err(err),
                        }
                    }
                }
            }
        }
        Ok(())
    }

    pub(crate) fn channel(&self) -> MsgSender {
        self.sender.clone()
    }

    fn run_event_loop(mut self) -> (Self, PtyState) {
        let mut state = PtyState::default();
        let mut buf = [0u8; READ_BUFFER_SIZE];

        // Token 0 is the reserved `Waker`; PTY source tokens start at 1.
        let mut tokens = (1_usize..).map(Token);

        // Register the PTY sources, handing them the loop `Waker` (Windows soft-ready;
        // ignored on Unix). mio 1.2 is edge-triggered; the loop re-registers interest
        // each pass to pick up write readiness.
        //
        // A registration failure is fatal for this session but must not panic
        // the reader thread (which would leave a frozen tab with no feedback);
        // report the terminal as closed instead, like the child-exit path.
        if let Err(err) =
            self.pty
                .register(&self.poll, &mut tokens, Interest::READABLE, &self.waker)
        {
            error!("Failed to register PTY event sources: {err}");

            self.event_proxy
                .send_event(TerminalEvent::CloseTerminal(self.route_id), self.window_id);

            self.event_proxy
                .send_event(TerminalEvent::Render, self.window_id);

            return (self, state);
        }

        let mut events = Events::with_capacity(1024);

        'event_loop: loop {
            // Windows soft-ready is level-like but lives outside the OS poll set,
            // and its worker only wakes on the clear→set edge. A `pty_read` capped
            // by MAX_LOCKED_READ can return with data still in the ring (flag left
            // set), so blocking with `None` would sleep forever on already-signalled
            // data. When a source is still ready, poll with a zero timeout to spin
            // back to `drain_ready` instead of sleeping. Unix returns `false` here
            // and keeps blocking (real OS readiness, re-armed by EPOLL_CTL_MOD).
            events.clear();

            let timeout = if self.pty.has_ready() {
                Some(time::Duration::ZERO)
            } else {
                self.sync_output_started_at
                    .map(|started| SYNC_OUTPUT_TIMEOUT.saturating_sub(started.elapsed()))
            };

            if let Err(err) = self.poll.poll(&mut events, timeout) {
                match err.kind() {
                    ErrorKind::Interrupted => continue,
                    _ => {
                        error!("Event loop polling error: {err}");
                        break 'event_loop;
                    }
                }
            }

            // Drain the `Msg` channel (resize/input/shutdown). It is a plain
            // `std::sync::mpsc` woken via the `Waker` (mio 1.2 has no pollable
            // channel), so it is drained on every wakeup rather than via a token.
            if !self.drain_recv_channel(&mut state) {
                break;
            }

            // Collect readiness from both sources: the Windows soft-ready set
            // (`drain_ready`) and real `Poll` events (Unix fds; on Windows `poll`
            // only ever yields the waker token). Both feed the same handling below.
            let mut do_read = false;
            let mut do_write = false;
            let mut child_exited = false;

            #[cfg(unix)]
            let mut hup = false;

            for token in self.pty.drain_ready() {
                if token == self.pty.read_token() {
                    do_read = true;
                } else if token == self.pty.write_token() {
                    do_write = true;
                } else if token == self.pty.child_event_token() {
                    child_exited = true;
                }
            }

            for event in events.iter() {
                let token = event.token();

                if token == self.pty.child_event_token() {
                    child_exited = true;
                } else if token == self.pty.read_token() || token == self.pty.write_token() {
                    #[cfg(unix)]
                    if event.is_read_closed() {
                        hup = true;
                    }

                    if event.is_readable() {
                        do_read = true;
                    }

                    if event.is_writable() {
                        do_write = true;
                    }
                }
                // The waker token (and any stray token) needs no handling.
            }

            if child_exited {
                if let Some(ChildEvent::Exited) = self.pty.next_child_event() {
                    // Emit `CloseTerminal` directly; PtyPipe owns the event proxy and route id.
                    self.event_proxy
                        .send_event(TerminalEvent::CloseTerminal(self.route_id), self.window_id);

                    self.event_proxy
                        .send_event(TerminalEvent::Render, self.window_id);

                    break 'event_loop;
                }
            }

            // Don't do I/O on a dead PTY (Unix HUP / `is_read_closed`).
            #[cfg(unix)]
            let skip_io = hup;
            #[cfg(not(unix))]
            let skip_io = false;

            if !skip_io {
                // A saturated batch self-wakes, while synchronized output uses the
                // poll deadline. Either path runs `pty_read` without new PTY bytes
                // so the pending snapshot can flush (it reads WouldBlock at worst).
                if do_read || self.snapshot_pending {
                    if let Err(err) = self.pty_read(&mut state, &mut buf) {
                        // On Linux, a `read` on the master side of a PTY can fail
                        // with `EIO` if the client side hangs up. In that case, just
                        // loop back round for the inevitable `Exited` event.
                        #[cfg(target_os = "linux")]
                        if err.raw_os_error() == Some(EIO) {
                            continue;
                        }

                        error!("Error reading from PTY in event loop: {}", err);
                        break 'event_loop;
                    }
                }

                if do_write {
                    if let Err(err) = self.pty_write(&mut state) {
                        error!("Error writing to PTY in event loop: {}", err);
                        break 'event_loop;
                    }
                }
            }

            // Re-register interest if a write is pending (real effect on Unix; the
            // Windows soft-ready path is a no-op).
            let mut interest = Interest::READABLE;

            if state.needs_write() {
                interest |= Interest::WRITABLE;
            }

            // Same as the registration above: fail the session visibly rather
            // than panicking the reader thread.
            if let Err(err) = self.pty.reregister(&self.poll, interest) {
                error!("Failed to reregister PTY event sources: {err}");

                self.event_proxy
                    .send_event(TerminalEvent::CloseTerminal(self.route_id), self.window_id);

                self.event_proxy
                    .send_event(TerminalEvent::Render, self.window_id);

                break 'event_loop;
            }
        }

        // The PTY sources are not dropped here, so deregister them explicitly.
        let _ = self.pty.deregister(&self.poll);

        (self, state)
    }

    pub(crate) fn spawn(self) -> JoinHandle<(Self, PtyState)> {
        Builder::new()
            .name("PTY reader".into())
            .spawn(move || self.run_event_loop())
            .expect("thread spawn works")
    }
}

#[cfg(test)]
mod scrollback_tests;

#[cfg(test)]
mod ghostty_mirror_tests;
