use std::mem;

use nmt_input::keyboard::ModifiersState;

use crate::surface::TerminalSurface;
use crate::surface::mouse::SurfaceCell;

impl TerminalSurface {
    pub(crate) fn apply_scroll(
        &self,
        cell: SurfaceCell,
        lines: i32,
        modifiers: ModifiersState,
    ) -> bool {
        if lines == 0 {
            return false;
        }

        if let Some(mode) = self.mouse_mode() {
            let button = if lines > 0 { 64 } else { 65 };

            return self.report_mouse(mode, button, true, cell.col, cell.row, modifiers);
        }

        self.scroll_lines(-(lines as isize))
    }

    pub(crate) fn scroll_lines(&self, delta: isize) -> bool {
        if delta == 0 {
            return false;
        }

        let before = self.session.render_buffer.lock().scrollbar();

        if before.total <= before.len {
            return false;
        }

        let snap = {
            let mut engine = self.session.engine.lock();
            engine.scroll_viewport_delta(delta);
            engine.snapshot()
        };

        let Ok(mut next) = snap else {
            return false;
        };

        let changed = next.scrollbar() != before;

        let mut buf = self.session.render_buffer.lock();

        next.set_cursor_visible(buf.cursor_visible());

        mem::swap(&mut *buf, &mut next);

        changed
    }
}
