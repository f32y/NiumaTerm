use std::mem;
use std::time::{Duration, Instant};

use nmt_platform::USES_CONPTY;
use nmt_platform::conpty_realign::{contains_csi_erase_display, realign_scroll_rows};

use crate::ghostty::{GhosttyTerminal, mode};

/// Reads after a resize during which ConPTY's repaint is still expected. Both
/// bounds must expire before the window closes: a burst of small reads must not
/// end it before the repaint arrives, and an idle shell must not keep it open.
const REPAINT_READS: u8 = 8;

const REPAINT_WINDOW: Duration = Duration::from_millis(150);

#[derive(Default)]
pub(super) struct ConptyResize {
    phase: ResizePhase,
}

#[derive(Default)]
enum ResizePhase {
    #[default]
    Idle,

    Armed {
        at: Instant,
        remaining: u8,

        /// Ghostty's cursor row and the PTY size latched when the resize was
        /// issued. Held until the one scroll this window may inject, so a
        /// keystroke echo arriving before the repaint cannot consume it.
        anchor: Option<ResizeAnchor>,

        /// An ED has been seen in this window: ConPTY's repaint is in flight.
        /// Spans reads because the erase and the repaint's CUPs may be split.
        erase_seen: bool,
    },
}

struct ResizeAnchor {
    row: u16,
    cols: u16,
    rows: u16,
}

#[derive(Default)]
pub(super) struct ResizeRead {
    pub(super) synthetic_prefix: Option<Vec<u8>>,
    pub(super) expected_cursor: Option<u16>,
    pub(super) repaint_window: bool,
}

impl ConptyResize {
    pub(super) fn on_resize(&mut self, cols: u16, rows: u16, active_row: Option<u16>, at: Instant) {
        if USES_CONPTY {
            self.phase = ResizePhase::Armed {
                at,
                remaining: REPAINT_READS,
                anchor: active_row.map(|row| ResizeAnchor { row, cols, rows }),
                erase_seen: false,
            };
        }
    }

    /// The PTY bytes always reach the engine unchanged. The only action this
    /// may take is to scroll the engine's content before them, and only once
    /// per window, once the burst is provably ConPTY's repaint (an ED has been
    /// seen and the CUPs fit the latched size). Rewriting CUP rows instead
    /// would leave ConPTY's cursor model and ghostty's pointing at different
    /// rows; ConPTY then addresses later echo by column only, and typed text
    /// keeps landing on a row the user cannot see.
    pub(super) fn on_read(&mut self, input: &[u8], engine: &mut GhosttyTerminal) -> ResizeRead {
        let ResizePhase::Armed {
            at,
            remaining,
            mut anchor,
            erase_seen,
        } = mem::take(&mut self.phase)
        else {
            return ResizeRead::default();
        };

        let mut read = ResizeRead {
            repaint_window: true,
            ..ResizeRead::default()
        };

        // A full-screen program owns the alternate screen; its redraw after a
        // resize is its own layout, not ConPTY's repaint of the primary screen.
        if engine.mode(mode::ALT_SCREEN) {
            return read;
        }

        let erase_seen = erase_seen || contains_csi_erase_display(input);

        if erase_seen
            && let Some(a) = &anchor
            && let Some((delta, target)) =
                realign_scroll_rows(input, a.row, a.cols, a.rows, engine.cols(), engine.rows())
        {
            let prefix = if delta > 0 {
                format!("\x1b[{delta}S")
            } else {
                format!("\x1b[{}T", -delta)
            }
            .into_bytes();

            engine.write_vt(&prefix);

            read.synthetic_prefix = Some(prefix);
            read.expected_cursor = Some(target);
            anchor = None;
        }

        let remaining = remaining.saturating_sub(1);

        self.phase = if remaining == 0 && at.elapsed() >= REPAINT_WINDOW {
            ResizePhase::Idle
        } else {
            ResizePhase::Armed {
                at,
                remaining,
                anchor,
                erase_seen,
            }
        };

        read
    }
}

#[cfg(all(test, windows))]
mod tests {
    use std::time::Instant;

    use crate::ghostty::GhosttyTerminal;
    use crate::pty_pipe::conpty_resize::ConptyResize;

    fn armed(engine: &mut GhosttyTerminal, prompt_row_1based: u16) -> ConptyResize {
        engine.write_vt(format!("\x1b[{prompt_row_1based};1H>").as_bytes());

        let mut resize = ConptyResize::default();

        resize.on_resize(80, 24, Some(prompt_row_1based - 1), Instant::now());

        resize
    }

    #[test]
    fn echo_before_repaint_keeps_anchor_and_repaint_scrolls_once() {
        let mut engine = GhosttyTerminal::new(80, 24, 100).unwrap();
        let mut resize = armed(&mut engine, 18);

        // Keystroke echo at ConPTY's stale row: no ED, so nothing happens and
        // the anchor survives for the repaint still to come.
        let echo = resize.on_read(b"\x1b[18;2Hx", &mut engine);

        assert!(echo.synthetic_prefix.is_none());
        assert!(echo.repaint_window);

        let repaint = b"\x1b[2;1H\x1b[J>";
        let first = resize.on_read(repaint, &mut engine);

        assert_eq!(
            first.synthetic_prefix.as_deref(),
            Some(b"\x1b[16S".as_slice())
        );
        assert_eq!(first.expected_cursor, Some(1));

        engine.write_vt(repaint);

        assert!(
            resize
                .on_read(repaint, &mut engine)
                .synthetic_prefix
                .is_none()
        );
    }

    #[test]
    fn erase_in_an_earlier_read_arms_the_scroll_for_the_cup_read() {
        let mut engine = GhosttyTerminal::new(80, 24, 100).unwrap();
        let mut resize = armed(&mut engine, 18);

        assert!(
            resize
                .on_read(b"\x1b[?25l\x1b[J", &mut engine)
                .synthetic_prefix
                .is_none()
        );

        assert_eq!(
            resize
                .on_read(b"\x1b[2;1H>", &mut engine)
                .synthetic_prefix
                .as_deref(),
            Some(b"\x1b[16S".as_slice())
        );
    }

    #[test]
    fn repaint_below_the_prompt_scrolls_down() {
        let mut engine = GhosttyTerminal::new(80, 24, 100).unwrap();
        let mut resize = armed(&mut engine, 2);

        assert_eq!(
            resize
                .on_read(b"\x1b[5;1H\x1b[J>", &mut engine)
                .synthetic_prefix
                .as_deref(),
            Some(b"\x1b[3T".as_slice())
        );
    }

    #[test]
    fn multi_row_redraw_without_erase_passes_through_untouched() {
        let mut engine = GhosttyTerminal::new(80, 24, 100).unwrap();
        let mut resize = armed(&mut engine, 18);

        // The shape of a PSReadLine ListView redraw: several rows, each
        // addressed absolutely and cleared with EL, cursor returned to the
        // input row. No ED anywhere, so it must not be taken for a repaint.
        let list =
            b"\x1b[18;1H\x1b[K> git\x1b[19;1H\x1b[K  git status\x1b[20;1H\x1b[K  git log\x1b[18;6H";

        let read = resize.on_read(list, &mut engine);

        assert!(read.synthetic_prefix.is_none());

        engine.write_vt(list);

        assert_eq!(engine.active_cursor_row(), Some(17));
    }

    #[test]
    fn alternate_screen_ends_the_window() {
        let mut engine = GhosttyTerminal::new(80, 24, 100).unwrap();
        let mut resize = armed(&mut engine, 18);

        engine.write_vt(b"\x1b[?1049h");

        assert!(
            resize
                .on_read(b"\x1b[2;1H\x1b[J>", &mut engine)
                .synthetic_prefix
                .is_none()
        );
        assert!(!resize.on_read(b"x", &mut engine).repaint_window);
    }
}
