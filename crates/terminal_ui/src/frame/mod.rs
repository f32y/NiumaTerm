use std::sync::Arc;

use nmt_terminal::ansi::CursorShape;
use nmt_terminal::ghostty::ScrollbarInfo;
use nmt_terminal::grid_emit::row_selection_for;
use nmt_terminal::render_buffer::RenderBuffer;
use nmt_terminal::selection::SelectionRange;

use crate::frame::images::empty_images;
use crate::pane_model::FrameTheme;

mod colors;
mod extract;
mod images;
mod line;

use crate::frame::colors::BackgroundColors;
#[cfg(test)]
pub(crate) use crate::frame::extract::extract_row;
use crate::frame::extract::{
    TerminalLineState, cursor_for_row, extract_row_with_colors, frame_cursor,
};
#[cfg(test)]
pub(crate) use crate::frame::images::FrameImageKind;
use crate::frame::images::extract_frame_images;
pub(crate) use crate::frame::images::{FrameImage, ZLayer};
#[cfg(test)]
pub(crate) use crate::frame::line::line_from_parts;
pub(crate) use crate::frame::line::{
    LineBuilder, StyleRun, TerminalCell, TerminalColor, TerminalLine,
};
use crate::pane_model::frame_cache::GenerationMap;

#[derive(Clone, Default)]
pub(crate) struct TerminalFrame {
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
pub(crate) struct TerminalCursor {
    pub(crate) col: u16,
    pub(crate) row: u16,
    pub(crate) shape: CursorShape,
    pub(crate) color: TerminalColor,
}

impl TerminalFrame {
    pub(crate) fn lines(&self) -> &[TerminalLine] {
        &self.lines
    }

    /// Paintable Kitty image placements for this frame.
    pub(crate) fn images(&self) -> &[FrameImage] {
        &self.images
    }

    pub(crate) fn cursor(&self) -> Option<TerminalCursor> {
        self.cursor
    }

    pub(crate) fn scrollbar(&self) -> ScrollbarInfo {
        self.scrollbar
    }

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
        Self::from_render_buffer_reusing(buf, selection, generations, None, &FrameTheme::default())
    }

    pub(crate) fn from_render_buffer_reusing(
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

#[cfg(test)]
mod tests;

/// Full-pipeline performance profile (manual, release-only). Puts every stage of
/// a fast-scrollback frame on ONE scale so engine-side costs can be compared
/// against the real render-thread cost.
///
/// ```text
/// ./scripts/profiling.ps1 test --release -p nmt_terminal_ui full_frame_profile '--' --ignored --nocapture
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
