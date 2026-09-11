use nmt_config::colors::term::{DIM_FACTOR, List, TermColors};
use nmt_config::colors::{AnsiColor, ColorArray, NamedColor};
use nmt_terminal::render_buffer::RenderBuffer;
use nmt_terminal::terminal::square::{ContentTag, Square};
use nmt_terminal::terminal::style::{Style, StyleFlags};

use crate::frame::TerminalColor;
use crate::pane_model::FrameTheme;

pub(super) struct BackgroundColors {
    colors: List,
    term_colors: TermColors,
    pub(super) selection_background: TerminalColor,
}

impl BackgroundColors {
    pub(super) fn new(term_colors: TermColors, theme: &FrameTheme) -> Self {
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
        match cell.content_tag() {
            ContentTag::BgRgb => {
                let (r, g, b) = cell.bg_rgb();

                Some((r, g, b).into())
            }

            ContentTag::BgPalette => Some(self.indexed(cell.bg_palette_index() as usize)),

            ContentTag::Codepoint => {
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

    pub(super) fn default_foreground(&self) -> TerminalColor {
        self.named(NamedColor::Foreground)
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
                    let color: ColorArray = (*rgb * DIM_FACTOR).into();

                    color.into()
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
