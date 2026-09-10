use nmt_config::CursorShape;
use nmt_config::colors::Colors;
use nmt_input::keyboard::ModifiersState;
use tracing::warn;

use crate::ghostty::{AcquiredBlock, BlockHandle, BlockRef, GhosttyTerminal};
use crate::graphics::GraphicData;
use crate::render_buffer::RenderBuffer;
use crate::session::TerminalSession;
use crate::terminal::pos::{Line, Pos};

impl TerminalSession {
    pub fn title(&self) -> String {
        self.engine.lock().title()
    }

    pub fn set_theme_colors(&self, colors: &Colors) {
        self.engine.lock().set_theme_colors(colors);
    }

    pub fn set_cursor_shape(&self, shape: CursorShape) -> bool {
        let next = {
            let mut engine = self.engine.lock();

            if let Err(error) = engine.set_default_cursor_shape(shape) {
                warn!("failed to update cursor shape: {error}");

                return false;
            }

            engine.snapshot()
        };

        match next {
            Ok(next) => {
                *self.render_buffer.lock() = next;

                true
            }
            Err(error) => {
                warn!("failed to refresh terminal after cursor shape change: {error}");

                false
            }
        }
    }

    /// Batch viewport reads hold only the render-buffer lock. The reader must
    /// not call other session operations while holding that lock.
    pub fn with_render_buffer<R>(&self, read: impl FnOnce(&RenderBuffer) -> R) -> R {
        read(&self.render_buffer.lock())
    }

    /// Batch history reads share one engine lock. The reader cannot mutate the
    /// engine and must not reenter the session or hold the block-store lock.
    pub fn with_screen_reader<R>(&self, read: impl FnOnce(&GhosttyTerminal) -> R) -> R {
        read(&self.engine.lock())
    }

    /// Acquire without holding the block-store lock: the PTY worker can acquire
    /// engine then block store. Reads through the returned reference are independent.
    pub fn acquire_block(&self, handle: BlockHandle) -> Option<AcquiredBlock> {
        self.engine.lock().acquire_block_snapshot(handle)
    }

    pub fn block_image_pixels(&self, block: &BlockRef, image_id: u32) -> Option<GraphicData> {
        self.engine.lock().block_image_pixels(block, image_id)
    }

    pub fn viewport_top_screen_row(&self) -> Option<u32> {
        self.engine.lock().viewport_top_screen()
    }

    pub(super) fn viewport_top(&self) -> i32 {
        self.viewport_top_screen_row().unwrap_or(0) as i32
    }

    pub(super) fn screen_pos(&self, pos: Pos) -> Pos {
        Pos::new(Line(pos.row.0 + self.viewport_top()), pos.col)
    }

    pub fn mouse_reporting_active(&self) -> bool {
        self.mouse_mode().is_some()
    }

    pub fn mouse_reporting_active_for(&self, modifiers: ModifiersState) -> bool {
        self.app_mouse_mode(modifiers).is_some()
    }
}
