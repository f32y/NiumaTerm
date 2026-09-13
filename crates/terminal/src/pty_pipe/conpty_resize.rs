use std::mem;
use std::time::{Duration, Instant};

use nmt_platform::USES_CONPTY;
use nmt_platform::conpty_realign::{
    is_conpty_resize_echo_input, is_conpty_resize_repaint, rewrite_conpty_resize_echo_cup_rows,
    su_realign_count,
};

use crate::ghostty::{GhosttyTerminal, mode};

const ECHO_WINDOW: Duration = Duration::from_millis(150);
const REPAINT_READS: u8 = 8;

#[derive(Default)]
pub(super) struct ConptyResize {
    phase: ResizePhase,
}

#[derive(Default)]
enum ResizePhase {
    #[default]
    Idle,

    Armed {
        window: ResizeWindow,
        anchor: Option<ResizeAnchor>,
    },

    Repainting {
        window: ResizeWindow,
        remaining: u8,
    },
}

struct ResizeWindow {
    at: Instant,
    echo_pending: bool,
}

struct ResizeAnchor {
    row: u16,
    cols: u16,
    rows: u16,
}

#[derive(Default)]
pub(super) struct ResizeRead {
    pub(super) rewritten: Option<Vec<u8>>,
    pub(super) synthetic_prefix: Option<Vec<u8>>,
    pub(super) expected_cursor: Option<u16>,
    pub(super) repaint_window: bool,
    pub(super) echo_pending: bool,
}

impl ConptyResize {
    pub(super) fn on_resize(&mut self, cols: u16, rows: u16, active_row: Option<u16>, at: Instant) {
        if USES_CONPTY {
            self.phase = ResizePhase::Armed {
                window: ResizeWindow {
                    at,
                    echo_pending: false,
                },
                anchor: active_row.map(|row| ResizeAnchor { row, cols, rows }),
            };
        }
    }

    pub(super) fn on_input(&mut self, input: &[u8], now: Instant) {
        let window = match &mut self.phase {
            ResizePhase::Idle => return,
            ResizePhase::Armed { window, .. } | ResizePhase::Repainting { window, .. } => window,
        };

        // A read-count bound alone stays open while the user is idle. Only
        // typing during the resize may need its stale cursor address shifted.
        if now.saturating_duration_since(window.at) < ECHO_WINDOW
            && is_conpty_resize_echo_input(input)
        {
            window.echo_pending = true;
        }
    }

    pub(super) fn on_read(&mut self, input: &[u8], engine: &mut GhosttyTerminal) -> ResizeRead {
        let (mut window, anchor, mut remaining) = match mem::take(&mut self.phase) {
            ResizePhase::Idle => return ResizeRead::default(),
            ResizePhase::Armed { window, anchor } => (window, anchor, REPAINT_READS),
            ResizePhase::Repainting { window, remaining } => (window, None, remaining),
        };

        let mut read = ResizeRead {
            repaint_window: remaining > 0,
            echo_pending: window.echo_pending,
            ..ResizeRead::default()
        };

        remaining = remaining.saturating_sub(1);

        // The first repaint uses the cursor captured at resize time. Later
        // reads have already moved that cursor, so they cannot repeat the
        // scroll-up adjustment. The size check rejects an unrelated repaint.
        if let Some(anchor) = anchor
            && !engine.mode(mode::ALT_SCREEN)
            && let Some((rows, target)) = su_realign_count(
                input,
                anchor.row,
                anchor.cols,
                anchor.rows,
                engine.cols(),
                engine.rows(),
            )
        {
            let prefix = format!("\x1b[{rows}S").into_bytes();

            engine.write_vt(&prefix);

            read.synthetic_prefix = Some(prefix);
            read.expected_cursor = Some(target);
            window.echo_pending = false;
        }

        if read.expected_cursor.is_none() && (window.echo_pending || read.repaint_window) {
            if engine.mode(mode::ALT_SCREEN) {
                window.echo_pending = false;
                remaining = 0;
            } else if let Some(active_row) = engine.active_cursor_row() {
                // CUP addresses the active screen, even when the visible
                // viewport is scrolled into history or has blank rows below it.
                let target_row = active_row.saturating_add(1);

                let repaint_pending =
                    read.repaint_window && is_conpty_resize_repaint(input, target_row);

                if window.echo_pending || repaint_pending {
                    read.rewritten = rewrite_conpty_resize_echo_cup_rows(input, target_row);
                }

                if read.rewritten.is_some() || repaint_pending {
                    window.echo_pending = false;

                    if repaint_pending {
                        remaining = 0;
                    }
                }
            }
        }

        self.phase = if remaining == 0 && !window.echo_pending && window.at.elapsed() >= ECHO_WINDOW
        {
            ResizePhase::Idle
        } else {
            ResizePhase::Repainting { window, remaining }
        };

        read
    }
}

#[cfg(all(test, windows))]
mod tests {
    use std::time::Instant;

    use crate::ghostty::GhosttyTerminal;
    use crate::pty_pipe::conpty_resize::{ConptyResize, ECHO_WINDOW, REPAINT_READS};

    #[test]
    fn first_repaint_scrolls_once_and_late_typing_is_not_rewritten() {
        let mut engine = GhosttyTerminal::new(80, 24, 100).unwrap();
        let mut resize = ConptyResize::default();
        let now = Instant::now();
        let repaint = b"\x1b[2;1H>";

        engine.write_vt(b"\x1b[18;1H>");
        resize.on_resize(80, 24, Some(17), now);

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

        for _ in 0..REPAINT_READS {
            resize.on_read(b"", &mut engine);
        }

        resize.on_input(b"x", now + ECHO_WINDOW);

        assert!(
            resize
                .on_read(b"\x1b[10;2Hx", &mut engine)
                .rewritten
                .is_none()
        );

        resize.on_resize(80, 24, None, now);
        resize.on_input(b"x", now);

        assert!(
            resize
                .on_read(b"\x1b[10;2Hx", &mut engine)
                .rewritten
                .is_some()
        );
    }
}
