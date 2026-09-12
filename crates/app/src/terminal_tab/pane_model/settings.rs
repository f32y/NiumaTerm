#[cfg(test)]
#[path = "settings_tests.rs"]
mod settings_tests;

use nmt_config::CursorShape;
use nmt_config::colors::Colors;
use nmt_config::colors::term::List;
use nmt_config::system::NewlineShortcut;
use nmt_terminal::session::request::Request;

use crate::terminal_tab::frame::TerminalColor;

pub(crate) struct CursorShapeUpdate {
    pub(super) request: Request<()>,
    pub(super) failure: CursorShapeFailure,
}

pub(crate) struct CursorShapeFailure {
    pub(super) previous: CursorShape,
    pub(super) requested: CursorShape,
}

impl CursorShapeUpdate {
    pub(crate) async fn failure(self) -> Option<CursorShapeFailure> {
        match self.request.await {
            Ok(Ok(())) => None,
            _ => Some(self.failure),
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct PaneSettings {
    pub fixed_bottom: bool,
    pub pad_rows: f32,
    pub show_block_chrome: bool,
    pub smooth_wheel: bool,
    pub scroll_to_bottom_when_typing: bool,
    pub newline_shortcut: NewlineShortcut,
    pub cursor_shape: CursorShape,
}

#[derive(Clone, Copy)]
pub(crate) struct FrameTheme {
    pub foreground: TerminalColor,
    pub background: TerminalColor,
    pub selection_background: TerminalColor,
    pub palette: List,
}

impl From<&Colors> for FrameTheme {
    fn from(colors: &Colors) -> Self {
        Self {
            foreground: colors.foreground.into(),
            background: colors.background.0.into(),
            selection_background: colors.selection_background.into(),
            palette: colors.into(),
        }
    }
}

#[cfg(test)]
impl Default for FrameTheme {
    fn default() -> Self {
        (&Colors::default()).into()
    }
}
