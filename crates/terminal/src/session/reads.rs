use std::sync::Arc;

use futures::channel::oneshot;
use nmt_config::CursorShape;
use nmt_config::colors::Colors;
use nmt_input::keyboard::ModifiersState;

use crate::event::Msg;
use crate::render_buffer::RenderBuffer;
use crate::session::TerminalSession;
use crate::session::request::Request;
use crate::terminal::pos::{Line, Pos};

impl TerminalSession {
    pub fn title(&self) -> String {
        self.snapshot().title.clone()
    }

    /// Reports queue acceptance; the owner publishes colors with its next frame.
    pub fn set_theme_colors(&self, colors: &Colors) -> bool {
        self.messenger.send(Msg::Theme(Box::new(*colors))).is_ok()
    }

    pub fn set_cursor_shape(&self, shape: CursorShape) -> Request<()> {
        let (reply, request) = oneshot::channel();
        let _ = self.messenger.send(Msg::CursorShape { shape, reply });
        request
    }

    pub fn snapshot(&self) -> Arc<RenderBuffer> {
        self.render_buffer.load()
    }

    pub fn with_render_buffer<R>(&self, read: impl FnOnce(&RenderBuffer) -> R) -> R {
        read(&self.snapshot())
    }

    pub fn viewport_top_screen_row(&self) -> Option<u32> {
        self.snapshot().viewport_top
    }

    pub(super) fn viewport_top(&self) -> i32 {
        self.viewport_top_screen_row()
            .unwrap_or(0)
            .min(i32::MAX as u32) as i32
    }

    pub(super) fn screen_pos(&self, pos: Pos) -> Pos {
        Pos::new(Line(pos.row.0.saturating_add(self.viewport_top())), pos.col)
    }

    pub fn mouse_reporting_active(&self) -> bool {
        self.mouse_mode().is_some()
    }

    pub fn mouse_reporting_active_for(&self, modifiers: ModifiersState) -> bool {
        self.app_mouse_mode(modifiers).is_some()
    }
}
