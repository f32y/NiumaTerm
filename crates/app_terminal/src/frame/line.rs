use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use gpui::SharedString;
use nmt_config::colors::ColorRgb;
use nmt_terminal::ansi::kitty_virtual::PLACEHOLDER;
use nmt_terminal::terminal::square::Wide;

#[derive(Clone)]
pub(crate) struct TerminalLine(Arc<TerminalLineData>);

struct TerminalLineData {
    text: SharedString,
    text_hash: u64,
    cells: Box<[TerminalCell]>,
    runs: Box<[StyleRun]>,
    #[cfg(test)]
    cursor_col: Option<u16>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TerminalCell {
    pub(crate) col: u16,
    pub(crate) ch: char,
    pub(crate) style_id: u16,
    pub(crate) background: Option<TerminalColor>,
    pub(crate) wide: Wide,
    pub(crate) extras: Vec<char>,
    pub(crate) has_cursor: bool,
}

pub(crate) type TerminalColor = ColorRgb;

/// A run of consecutive cells sharing one foreground style, in row order.
/// `len` is the UTF-8 byte length this run contributes to the row text, so the
/// runs line up 1:1 with the shaped line's bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StyleRun {
    pub(crate) len: usize,
    pub(crate) fg: TerminalColor,
    pub(crate) bold: bool,
    pub(crate) italic: bool,
    pub(crate) underline: bool,
    pub(crate) strikethrough: bool,
}

impl TerminalLine {
    pub(crate) fn text(&self) -> &SharedString {
        &self.0.text
    }

    pub(crate) fn cells(&self) -> &[TerminalCell] {
        &self.0.cells
    }

    pub(crate) fn runs(&self) -> &[StyleRun] {
        &self.0.runs
    }

    pub(crate) fn text_hash(&self) -> u64 {
        self.0.text_hash
    }

    #[cfg(test)]
    pub(crate) fn cursor_col(&self) -> Option<u16> {
        self.0.cursor_col
    }

    fn new(
        text: String,
        cells: Vec<TerminalCell>,
        runs: Vec<StyleRun>,
        cursor_col: Option<u16>,
    ) -> Self {
        let text_hash = hash_line(&text, &runs);
        let _ = cursor_col;

        Self(Arc::new(TerminalLineData {
            text: text.into(),
            text_hash,
            cells: cells.into_boxed_slice(),
            runs: runs.into_boxed_slice(),
            #[cfg(test)]
            cursor_col,
        }))
    }

    #[cfg(test)]
    pub(super) fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// Build a display line from pre-resolved parts (block-split frozen history:
/// the cells were harvested with resolved colors, so no RenderBuffer/engine
/// lookup is involved). The hash folds text + runs, so frozen lines hit the
/// shaped-line cache forever.
pub(crate) fn line_from_parts(
    text: String,
    cells: Vec<TerminalCell>,
    runs: Vec<StyleRun>,
) -> TerminalLine {
    TerminalLine::new(text, cells, runs, None)
}

/// Accumulates display cells into a `TerminalLine` — the one display-
/// convention kernel for the live frame extractor and the frozen engine-row
/// builder: appends display text, merges runs of equal style, and gives wide
/// glyphs an NBSP placeholder column (GPUI's force-width layout snaps one
/// glyph per cell, so without it a wide glyph overlaps the next cell).
#[derive(Default)]
pub(crate) struct LineBuilder {
    text: String,
    cells: Vec<TerminalCell>,
    runs: Vec<StyleRun>,
}

impl LineBuilder {
    pub(crate) fn with_capacity(cols: usize) -> Self {
        Self {
            text: String::with_capacity(cols),
            cells: Vec::with_capacity(cols),
            runs: Vec::new(),
        }
    }

    /// Append one cell's display text; `wide` adds the placeholder column,
    /// covered by the same run. `style.len` is ignored — the run length is
    /// the appended byte count, merged into the previous run on equal style.
    pub(crate) fn push_segment(
        &mut self,
        display: impl Iterator<Item = char>,
        style: StyleRun,
        wide: bool,
    ) {
        let start = self.text.len();

        self.text.extend(display);

        if wide {
            self.text.push('\u{00a0}');
        }

        let seg_len = self.text.len() - start;

        match self.runs.last_mut() {
            Some(last)
                if StyleRun {
                    len: last.len,
                    ..style
                } == *last =>
            {
                last.len += seg_len
            }
            _ => self.runs.push(StyleRun {
                len: seg_len,
                ..style
            }),
        }
    }

    /// Record the cell for background/hit lookups. Separate from
    /// `push_segment` because filler columns (gaps between sparse engine
    /// cells) contribute text but no cell.
    pub(crate) fn push_cell(&mut self, cell: TerminalCell) {
        self.cells.push(cell);
    }

    pub(crate) fn finish(self) -> TerminalLine {
        line_from_parts(self.text, self.cells, self.runs)
    }

    pub(super) fn finish_with_cursor(self, cursor_col: Option<u16>) -> TerminalLine {
        TerminalLine::new(self.text, self.cells, self.runs, cursor_col)
    }
}

pub(super) fn display_char(ch: char) -> char {
    // Kitty virtual-placeholder cells (U+10EEEE) carry image slices, not glyphs;
    // render them as a blank so GPUI never draws a missing-glyph box while the cell
    // keeps its width for image geometry.
    if ch == '\0' || ch == '\t' || ch == ' ' || ch == PLACEHOLDER {
        '\u{00a0}'
    } else {
        ch
    }
}

fn hash_line(text: &str, runs: &[StyleRun]) -> u64 {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);

    // Fold style into the key so a same-text/different-color row invalidates the
    // shaped-line cache (otherwise recolored output would keep stale glyph runs).
    for run in runs {
        run.len.hash(&mut hasher);
        run.fg.r.hash(&mut hasher);
        run.fg.g.hash(&mut hasher);
        run.fg.b.hash(&mut hasher);
        run.bold.hash(&mut hasher);
        run.italic.hash(&mut hasher);
        run.underline.hash(&mut hasher);
        run.strikethrough.hash(&mut hasher);
    }

    hasher.finish()
}
