use nmt_config::active_colors;
use nmt_config::colors::term::{DIM_FACTOR, List, TermColors};
use nmt_config::colors::{AnsiColor, NamedColor};
use nmt_terminal::grid_emit::RowSelection;
use nmt_terminal::render_buffer::RenderBuffer;
use nmt_terminal::terminal::square::{ContentTag, Square};
use nmt_terminal::terminal::style::{Style, StyleFlags};

use super::TerminalColor;

pub(super) struct BackgroundColors {
    colors: List,
    term_colors: TermColors,
    pub(super) selection_background: TerminalColor,
}

impl BackgroundColors {
    pub(super) fn new(term_colors: TermColors) -> Self {
        // Active theme from config (loader resolves the theme/adaptive palette);
        // `term_colors` still overrides per-index via engine OSC 4 changes.
        let colors = active_colors();
        Self {
            colors: List::from(&colors),
            term_colors,
            selection_background: TerminalColor::from_color_arr(colors.selection_background),
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
                self.style_background(style)
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

    fn style_background(&self, style: Style) -> Option<TerminalColor> {
        if style.flags.contains(StyleFlags::INVERSE) {
            Some(self.color(&style.fg, style.flags, true))
        } else {
            match style.bg {
                AnsiColor::Named(NamedColor::Background) => None,
                _ => Some(self.color(&style.bg, style.flags, false)),
            }
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
                    TerminalColor::from_color_arr((*rgb * DIM_FACTOR).to_arr())
                } else {
                    (*rgb).into()
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
        TerminalColor::from_color_arr(self.term_colors[index].unwrap_or(self.colors[index]))
    }
}

/// The theme's default foreground, for harvested cells with no explicit fg
/// (block-split).
pub(crate) fn theme_default_foreground() -> TerminalColor {
    TerminalColor::from_color_arr(active_colors().foreground)
}

pub(crate) fn theme_default_background() -> TerminalColor {
    TerminalColor::from_color_arr(active_colors().background.0)
}

/// The theme's selection background (block-split frozen selection).
pub(crate) fn theme_selection_background() -> TerminalColor {
    TerminalColor::from_color_arr(active_colors().selection_background)
}

pub(super) fn cell_is_selected(row_selection: Option<RowSelection>, col: u16) -> bool {
    row_selection.is_some_and(|selection| col >= selection.lo && col <= selection.hi)
}
