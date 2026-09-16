use nmt_config::colors::term::{DIM_FACTOR, List, TermColors};
use nmt_config::colors::{AnsiColor, ColorArray, NamedColor};
use nmt_terminal::ghostty::SnapshotStyle;
use nmt_terminal::grid::{Square, Style, StyleFlags};
use nmt_terminal::render_buffer::RenderBuffer;

use crate::terminal_tab::frame::TerminalColor;
use crate::terminal_tab::pane_model::FrameTheme;

/// Resolves cell colors for display. The viewport hands over interned
/// `Style` values with palette references; rows read from the engine
/// (scrollback and frozen blocks) hand over `SnapshotStyle` values whose
/// palette entries the engine already resolved to RGB. Both go through the
/// same default-color override layer, inverse swap, and dim factor so a row
/// looks the same whether it is still in the viewport or has scrolled out.
pub(crate) struct BackgroundColors {
    colors: List,
    term_colors: TermColors,
    pub(super) selection_background: TerminalColor,
}

impl BackgroundColors {
    pub(crate) fn new(term_colors: TermColors, theme: &FrameTheme) -> Self {
        Self {
            colors: theme.palette,
            term_colors,
            selection_background: theme.selection_background,
        }
    }

    pub(super) fn cell_background(
        &self,
        buf: &RenderBuffer,
        cell: Square,
    ) -> Option<TerminalColor> {
        let style = buf.style(cell.style_id());

        if style.flags.contains(StyleFlags::INVERSE) {
            Some(self.color(&style.fg, style.flags, true))
        } else {
            match style.bg {
                AnsiColor::Named(NamedColor::Background) => None,
                _ => Some(self.color(&style.bg, style.flags, false)),
            }
        }
    }

    pub(super) fn cell_foreground(&self, style: Style) -> TerminalColor {
        // Inverse swaps fg/bg: the painted text takes the background color.
        if style.flags.contains(StyleFlags::INVERSE) {
            match style.bg {
                AnsiColor::Named(NamedColor::Background) => self.named(NamedColor::Background),
                _ => self.color(&style.bg, style.flags, false),
            }
        } else {
            self.color(&style.fg, style.flags, true)
        }
    }

    /// Foreground of a cell read from the engine. Palette identity is gone
    /// by then, so the viewport's bold brightening and dim remap of indexed
    /// colors cannot apply; the default colors and faint text follow the
    /// same rules as the viewport.
    pub(super) fn engine_foreground(&self, style: &SnapshotStyle) -> TerminalColor {
        if style.inverse {
            style
                .bg
                .unwrap_or_else(|| self.named(NamedColor::Background))
        } else {
            self.engine_text_color(style)
        }
    }

    pub(super) fn engine_background(&self, style: &SnapshotStyle) -> Option<TerminalColor> {
        if style.inverse {
            Some(self.engine_text_color(style))
        } else {
            style.bg
        }
    }

    fn engine_text_color(&self, style: &SnapshotStyle) -> TerminalColor {
        match (style.fg, style.faint) {
            (Some(fg), true) => dim(fg),
            (Some(fg), false) => fg,
            (None, true) => self.named(NamedColor::DimForeground),
            (None, false) => self.named(NamedColor::Foreground),
        }
    }

    fn color(&self, color: &AnsiColor, flags: StyleFlags, foreground: bool) -> TerminalColor {
        let dim = foreground && flags.contains(StyleFlags::DIM);
        let bold = foreground && flags.contains(StyleFlags::BOLD);

        match color {
            AnsiColor::Named(named) => {
                let named = if foreground && bold && !dim {
                    named.to_light()
                } else if dim {
                    named.to_dim()
                } else {
                    *named
                };

                self.named(named)
            }
            AnsiColor::Spec(rgb) => {
                if dim {
                    self::dim(*rgb)
                } else {
                    *rgb
                }
            }
            AnsiColor::Indexed(index) => {
                let index = match (foreground, dim, bold, *index) {
                    (true, true, _, 8..=15) => *index as usize - 8,
                    (true, true, _, 0..=7) => NamedColor::DimBlack as usize + *index as usize,
                    (false, false, true, 0..=7) => *index as usize + 8,
                    (false, true, false, 8..=15) => *index as usize - 8,
                    (false, true, false, 0..=7) => NamedColor::DimBlack as usize + *index as usize,
                    _ => *index as usize,
                };

                self.indexed(index)
            }
        }
    }

    pub(super) fn named(&self, named: NamedColor) -> TerminalColor {
        self.indexed(named as usize)
    }

    fn indexed(&self, index: usize) -> TerminalColor {
        self.term_colors[index].unwrap_or(self.colors[index]).into()
    }
}

fn dim(color: TerminalColor) -> TerminalColor {
    let color: ColorArray = (color * DIM_FACTOR).into();

    color.into()
}
