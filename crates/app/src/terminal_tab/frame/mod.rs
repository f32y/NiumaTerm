#[cfg(test)]
pub(super) use crate::terminal_tab::frame::images::FrameImageKind;
pub(super) use crate::terminal_tab::frame::images::{FrameImage, ZLayer};
#[cfg(test)]
pub(super) use crate::terminal_tab::frame::line::line_from_parts;
pub(super) use crate::terminal_tab::frame::line::{
    EngineRowBuilder, LineBuilder, StyleRun, TerminalCell, TerminalColor, TerminalLine,
};

mod colors;
mod images;
mod line;

/// Full-pipeline performance profile (manual, release-only). Puts every stage of
/// a fast-scrollback frame on ONE scale so engine-side costs can be compared
/// against the real render-thread cost.
///
/// ```text
/// ./scripts/profiling.ps1 test --release -p app --lib full_frame_profile '--' --ignored --nocapture
/// ```
///
/// Stages, in pipeline order:
///   1. parse — `engine.write_vt` of 20k distinct 72-col lines (runs on the
///      PTY thread today, off the frame critical path).
///   2. snapshot — `engine.snapshot` of the live viewport (once per rendered frame).
///   3. extract — forced full extraction plus a one-row incremental update of
///      the viewport (the live-region materialization, render thread).
///   4. shape — real DirectWrite `layout_line` of NOVEL lines (render thread,
///      cache-miss cost). Production caches shaped lines by hash, so
///      repeated output is ~free; novel output pays this per line.
///
/// GPU submission is excluded (GPUI's own bench harness excludes it off-macOS).
#[cfg(all(test, enable_profiling))]
mod full_frame_profile;

#[cfg(test)]
mod tests;

use std::iter;
use std::sync::Arc;

use nmt_config::colors::NamedColor;
use nmt_terminal::ansi::CursorShape;
use nmt_terminal::ghostty::ScrollbarInfo;
use nmt_terminal::grid_emit::{RowSelection, row_selection_for};
use nmt_terminal::render_buffer::RenderBuffer;
use nmt_terminal::selection::SelectionRange;
use nmt_terminal::terminal::square::{ContentTag, Wide};
use nmt_terminal::terminal::style::StyleFlags;

use crate::terminal_tab::frame::colors::BackgroundColors;
use crate::terminal_tab::frame::images::{empty_images, extract_frame_images};
use crate::terminal_tab::frame::line::display_char;
use crate::terminal_tab::pane_model::FrameTheme;
use crate::terminal_tab::pane_model::frame_cache::GenerationMap;

#[derive(Clone, Default)]
pub(super) struct TerminalFrame {
    lines: Arc<[TerminalLine]>,
    line_states: Arc<[TerminalLineState]>,
    cols: usize,
    cursor: Option<TerminalCursor>,
    scrollbar: ScrollbarInfo,

    /// Paintable Kitty image placements resolved against the session image cache
    /// Empty in the common no-graphics case.
    images: Arc<[FrameImage]>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct TerminalCursor {
    pub(super) col: u16,
    pub(super) row: u16,
    pub(super) shape: CursorShape,
    pub(super) color: TerminalColor,
}

impl TerminalFrame {
    pub(super) fn lines(&self) -> &[TerminalLine] {
        &self.lines
    }

    /// Paintable Kitty image placements for this frame.
    pub(super) fn images(&self) -> &[FrameImage] {
        &self.images
    }

    pub(super) fn cursor(&self) -> Option<TerminalCursor> {
        self.cursor
    }

    pub(super) fn scrollbar(&self) -> ScrollbarInfo {
        self.scrollbar
    }

    #[cfg(test)]
    pub(super) fn from_render_buffer(buf: &RenderBuffer) -> Self {
        Self::from_render_buffer_with_selection(buf, None, &GenerationMap::new())
    }

    #[cfg(test)]
    pub(super) fn from_render_buffer_with_selection(
        buf: &RenderBuffer,
        selection: Option<SelectionRange>,
        generations: &GenerationMap,
    ) -> Self {
        Self::from_render_buffer_reusing(buf, selection, generations, None, &FrameTheme::default())
    }

    pub(super) fn from_render_buffer_reusing(
        buf: &RenderBuffer,
        selection: Option<SelectionRange>,
        generations: &GenerationMap,
        previous: Option<&Self>,
        theme: &FrameTheme,
    ) -> Self {
        let colors = BackgroundColors::new(buf.colors(), theme);
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct TerminalLineState {
    version: u64,
    selection: Option<RowSelection>,
}

#[cfg(test)]
pub(super) fn extract_row(
    buf: &RenderBuffer,
    row: usize,
    cursor: Option<TerminalCursor>,
) -> TerminalLine {
    let colors = BackgroundColors::new(buf.colors(), &FrameTheme::default());

    extract_row_with_colors(buf, row, cursor, &colors, None)
}

fn extract_row_with_colors(
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

        let background = if row_selection
            .is_some_and(|selection| col as u16 >= selection.lo && col as u16 <= selection.hi)
        {
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

fn frame_cursor(buf: &RenderBuffer, colors: &BackgroundColors) -> Option<TerminalCursor> {
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

fn cursor_for_row(cursor: Option<TerminalCursor>, row: usize) -> Option<TerminalCursor> {
    cursor.filter(|cursor| cursor.row as usize == row)
}
