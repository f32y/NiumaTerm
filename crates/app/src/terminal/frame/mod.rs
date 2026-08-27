use std::sync::Arc;

use nmt_terminal::ansi::CursorShape;
use nmt_terminal::ghostty::ScrollbarInfo;

mod cache;
mod colors;
mod extract;
mod images;
mod line;

#[cfg(test)]
use cache::GenerationMap;
pub(crate) use cache::TerminalFrameCache;
#[cfg(test)]
use colors::BackgroundColors;
pub(crate) use colors::{
    theme_default_background, theme_default_foreground, theme_selection_background,
};
use extract::TerminalLineState;
#[cfg(test)]
pub(crate) use extract::extract_row;
#[cfg(test)]
use extract::{cursor_for_row, extract_row_with_colors, frame_cursor};
pub(crate) use images::{FrameImage, ZLayer};
#[cfg(test)]
pub(crate) use images::{FrameImageKind, extract_frame_images};
#[cfg(test)]
pub(crate) use line::line_from_parts;
pub(crate) use line::{LineBuilder, StyleRun, TerminalCell, TerminalColor, TerminalLine};

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
}

#[cfg(test)]
mod tests;

/// Full-pipeline performance profile (manual, release-only). Puts every stage of
/// a fast-scrollback frame on ONE scale so engine-side costs can be compared
/// against the real render-thread cost.
///
/// ```text
/// cargo test --release -p app full_frame_profile -- --ignored --nocapture
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
#[cfg(test)]
mod full_frame_profile;
