use nmt_input::keyboard::ModifiersState;

use crate::event::Msg;
use crate::session::TerminalSession;
use crate::session::mouse::SurfaceCell;

impl TerminalSession {
    pub fn scroll_to(&self, offset: u64) -> bool {
        !self.exited() && self.messenger.send(Msg::ScrollTo(offset)).is_ok()
    }

    pub fn scroll_to_end(&self) -> bool {
        !self.exited() && self.messenger.send(Msg::ScrollToEnd).is_ok()
    }

    pub fn apply_scroll(&self, cell: SurfaceCell, lines: i32, modifiers: ModifiersState) -> bool {
        if lines == 0 {
            return false;
        }
        if let Some(mode) = self.mouse_mode() {
            let button = if lines > 0 { 64 } else { 65 };
            return self.report_mouse(mode, button, true, cell.col, cell.row, modifiers);
        }
        self.scroll_lines(-(lines as isize))
    }

    /// A successful send accepts the scroll request. The next published frame
    /// determines the resulting position, including clamping at history edges.
    pub fn scroll_lines(&self, delta: isize) -> bool {
        if delta == 0 || self.exited() {
            return false;
        }
        let before = self.snapshot().scrollbar();
        if before.total <= before.len {
            return false;
        }
        self.messenger.send(Msg::Scroll(delta)).is_ok()
    }
}
