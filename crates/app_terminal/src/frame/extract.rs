use std::iter;

use nmt_config::colors::NamedColor;
use nmt_terminal::ansi::CursorShape;
use nmt_terminal::grid_emit::{RowSelection, row_selection_for};
use nmt_terminal::render_buffer::RenderBuffer;
use nmt_terminal::selection::SelectionRange;
use nmt_terminal::terminal::square::{ContentTag, Wide};
use nmt_terminal::terminal::style::StyleFlags;

use super::cache::GenerationMap;
use super::colors::{BackgroundColors, cell_is_selected};
use super::images::{empty_images, extract_frame_images};
use super::line::{LineBuilder, StyleRun, TerminalCell, TerminalLine, display_char};
use super::{TerminalCursor, TerminalFrame};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct TerminalLineState {
    version: u64,
    selection: Option<RowSelection>,
}

impl TerminalFrame {
    #[cfg(test)]
    pub(crate) fn from_render_buffer(buf: &RenderBuffer) -> Self {
        Self::from_render_buffer_with_selection(buf, None, &GenerationMap::new())
    }

    #[cfg(test)]
    pub(crate) fn from_render_buffer_with_selection(
        buf: &RenderBuffer,
        selection: Option<SelectionRange>,
        generations: &GenerationMap,
    ) -> Self {
        Self::from_render_buffer_reusing(buf, selection, generations, None)
    }

    pub(crate) fn from_render_buffer_reusing(
        buf: &RenderBuffer,
        selection: Option<SelectionRange>,
        generations: &GenerationMap,
        previous: Option<&Self>,
    ) -> Self {
        let colors = BackgroundColors::new(buf.colors());
        let cursor = frame_cursor(buf, &colors);
        let reusable = previous.filter(|frame| {
            frame.cols == buf.cols()
                && frame.lines.len() == buf.rows()
                && frame.line_states.len() == buf.rows()
                && buf.row_versions().len() == buf.rows()
        });

        let mut lines = Vec::with_capacity(buf.rows());
        let mut line_states = Vec::with_capacity(buf.rows());

        for row in 0..buf.rows() {
            let state = TerminalLineState {
                version: buf.row_versions().get(row).copied().unwrap_or_default(),
                selection: row_selection_for(selection, row, buf.cols()),
            };

            let row_cursor = cursor_for_row(cursor, row);

            let line = reusable
                .filter(|frame| {
                    frame.line_states[row] == state
                        && cursor_for_row(frame.cursor, row) == row_cursor
                })
                .map_or_else(
                    || extract_row_with_colors(buf, row, row_cursor, &colors, state.selection),
                    |frame| frame.lines[row].clone(),
                );

            lines.push(line);

            line_states.push(state);
        }

        // Reuse one shared empty `Arc` for the common no-image frame so a graphics-free
        // rebuild allocates nothing for `images` (an empty `Vec::into::<Arc<[_]>>()`
        // still allocates the Arc header).
        let images_vec = extract_frame_images(buf, generations);

        let images = if images_vec.is_empty() {
            empty_images()
        } else {
            images_vec.into()
        };

        Self {
            lines: lines.into_boxed_slice().into(),
            line_states: line_states.into_boxed_slice().into(),
            cols: buf.cols(),
            cursor,
            scrollbar: buf.scrollbar(),
            images,
        }
    }
}

#[cfg(test)]
pub(crate) fn extract_row(
    buf: &RenderBuffer,
    row: usize,
    cursor: Option<TerminalCursor>,
) -> TerminalLine {
    let colors = BackgroundColors::new(buf.colors());
    extract_row_with_colors(buf, row, cursor, &colors, None)
}

pub(super) fn extract_row_with_colors(
    buf: &RenderBuffer,
    row: usize,
    cursor: Option<TerminalCursor>,
    colors: &BackgroundColors,
    row_selection: Option<RowSelection>,
) -> TerminalLine {
    let mut builder = LineBuilder::with_capacity(buf.cols());

    for col in 0..buf.cols() {
        let cell = buf.cell(col, row);
        let wide = cell.wide();

        if matches!(wide, Wide::Spacer | Wide::LeadingSpacer) {
            continue;
        }

        let is_codepoint = cell.content_tag() == ContentTag::Codepoint;
        let source_ch = if is_codepoint { cell.c() } else { '\0' };
        let cursor_shape = cursor
            .filter(|cursor| cursor.col == col as u16)
            .map(|cursor| cursor.shape);

        let background = if cell_is_selected(row_selection, col as u16) {
            Some(colors.selection_background)
        } else {
            colors.cell_background(buf, cell)
        };

        let extras = if is_codepoint {
            cell.extras_id()
                .and_then(|extras_id| buf.extras().get(&extras_id))
                .map(|extras| extras.zerowidth.clone())
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        let mut style = if is_codepoint {
            let style = buf.style(cell.style_id());
            let flags = style.flags;
            StyleRun {
                len: 0,
                fg: colors.cell_foreground(style),
                bold: flags.contains(StyleFlags::BOLD),
                italic: flags.contains(StyleFlags::ITALIC),
                underline: flags.intersects(StyleFlags::ALL_UNDERLINES),
                strikethrough: flags.contains(StyleFlags::STRIKEOUT),
            }
        } else {
            StyleRun {
                len: 0,
                fg: colors.default_foreground(),
                bold: false,
                italic: false,
                underline: false,
                strikethrough: false,
            }
        };

        if cursor_shape == Some(CursorShape::Block) {
            // An opaque block replaces the cell background, so painting its glyph
            // with that original background preserves inverse-video contrast.
            style.fg = background.unwrap_or_else(|| colors.named(NamedColor::Background));
        }

        builder.push_segment(
            iter::once(display_char(source_ch)).chain(extras.iter().copied()),
            style,
            wide == Wide::Wide,
        );

        builder.push_cell(TerminalCell {
            col: col as u16,
            ch: source_ch,
            style_id: if is_codepoint { cell.style_id() } else { 0 },
            background,
            wide,
            extras,
            has_cursor: cursor_shape.is_some(),
        });
    }

    builder.finish_with_cursor(cursor.map(|cursor| cursor.col))
}

pub(super) fn frame_cursor(
    buf: &RenderBuffer,
    colors: &BackgroundColors,
) -> Option<TerminalCursor> {
    let cursor = buf.cursor();
    let shape = buf.cursor_shape();
    (buf.cursor_visible() && cursor.row.0 >= 0 && shape != CursorShape::Hidden).then_some(
        TerminalCursor {
            col: cursor.col.0.min(u16::MAX as usize) as u16,
            row: (cursor.row.0 as usize).min(u16::MAX as usize) as u16,
            shape,
            color: colors.named(NamedColor::Cursor),
        },
    )
}

pub(super) fn cursor_for_row(cursor: Option<TerminalCursor>, row: usize) -> Option<TerminalCursor> {
    cursor.filter(|cursor| cursor.row as usize == row)
}
