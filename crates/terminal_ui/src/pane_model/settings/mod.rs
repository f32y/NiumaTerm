use nmt_config::CursorShape;
use nmt_config::colors::Colors;
use nmt_config::colors::term::List;
use nmt_config::system::NewlineShortcut;
use nmt_terminal::session::request::Request;

use crate::block_list::chrome::DurationLabels;
use crate::frame::TerminalColor;
use crate::pane_model::PaneController;

pub(crate) struct CursorShapeUpdate {
    request: Request<()>,
    failure: CursorShapeFailure,
}

pub(crate) struct CursorShapeFailure {
    previous: CursorShape,
    requested: CursorShape,
}

impl CursorShapeUpdate {
    pub(crate) async fn failure(self) -> Option<CursorShapeFailure> {
        match self.request.await {
            Ok(Ok(())) => None,
            _ => Some(self.failure),
        }
    }
}

impl PaneController {
    pub(crate) fn update_settings(
        &mut self,
        settings: PaneSettings,
        colors: &Colors,
        duration_labels: DurationLabels,
    ) -> Option<CursorShapeUpdate> {
        self.source.session.set_theme_colors(colors);
        let cursor_update =
            (settings.cursor_shape != self.settings.cursor_shape).then(|| CursorShapeUpdate {
                request: self.source.session.set_cursor_shape(settings.cursor_shape),
                failure: CursorShapeFailure {
                    previous: self.settings.cursor_shape,
                    requested: settings.cursor_shape,
                },
            });
        self.settings = settings;
        self.theme = FrameTheme::from(colors);
        self.duration_labels = duration_labels;
        self.cell_metrics = None;
        self.frame_cache.invalidate_full();
        cursor_update
    }

    pub(crate) fn cursor_shape_failed(&mut self, failure: CursorShapeFailure) -> bool {
        if self.settings.cursor_shape != failure.requested {
            return false;
        }
        self.settings.cursor_shape = failure.previous;
        self.invalidate();
        true
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
            foreground: TerminalColor::from_color_arr(colors.foreground),
            background: TerminalColor::from_color_arr(colors.background.0),
            selection_background: TerminalColor::from_color_arr(colors.selection_background),
            palette: List::from(colors),
        }
    }
}

#[cfg(test)]
impl Default for FrameTheme {
    fn default() -> Self {
        Self::from(&Colors::default())
    }
}

#[cfg(test)]
mod tests;
