// Retired from: https://github.com/alacritty/alacritty/blob/6e7f466c68b387f41726757eed4f3e70d05479d2/alacritty_terminal/src/selection.rs
// which is licensed under Apache 2.0 license.
//! State management for a selection in the grid.
//!
//! A selection should start when the mouse is clicked, and it should be
//! finalized when the button is released. The selection should be cleared
//! when text is added/removed/scrolled on the screen. The selection should
//! also be cleared if the user clicks off of the selection.

#[cfg(test)]
#[path = "selection_tests.rs"]
mod selection_tests;

use std::mem;
use std::ops::{Bound, Range, RangeBounds};

use crate::grid::{Column, Line, Pos, Row, Side, Square, Wide};
use crate::render_buffer::RenderBuffer;

/// Characters that split words for semantic selection. Matches Windows
/// Terminal's default so paths, flags, and punctuation select predictably.
pub const WORD_DELIMITERS: &str = " ./\\()\"'-:,.;<>~!@#$%^&*|+=[]{}~?\u{2502}\t\0";

/// A Pos and side within that point.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct Anchor {
    pub point: Pos,
    side: Side,
}

impl Anchor {
    fn new(point: Pos, side: Side) -> Anchor {
        Anchor { point, side }
    }
}

/// Represents a range of selected cells.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct SelectionRange {
    /// Start point, top left of the selection.
    pub start: Pos,

    /// End point, bottom right of the selection.
    pub end: Pos,

    /// Whether this selection is a block selection.
    pub is_block: bool,
}

impl SelectionRange {
    pub fn new(start: Pos, end: Pos, is_block: bool) -> Self {
        assert!(start <= end);

        Self {
            start,
            end,
            is_block,
        }
    }
}

/// Different kinds of selection.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum SelectionType {
    Simple,
    Block,
    Semantic,
    Lines,
}

/// Describes a region of a 2-dimensional area.
///
/// Used to track a text selection. There are four supported modes, each with its own constructor:
/// [`simple`], [`block`], [`semantic`], and [`lines`]. The [`simple`] mode precisely tracks which
/// cells are selected without any expansion. [`block`] will select rectangular regions.
/// [`lines`] will always select entire lines.
///
/// Calls to [`update`] operate different based on the selection kind. The [`simple`] and [`block`]
/// mode do nothing special, simply track points and sides.
///
/// [`simple`]: enum.Selection.html#method.simple
/// [`block`]: enum.Selection.html#method.block
/// [`lines`]: enum.Selection.html#method.rows
/// [`update`]: enum.Selection.html#method.update
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub ty: SelectionType,
    region: Range<Anchor>,
}

impl Selection {
    pub fn new(ty: SelectionType, location: Pos, side: Side) -> Selection {
        Self {
            region: Range {
                start: Anchor::new(location, side),
                end: Anchor::new(location, side),
            },
            ty,
        }
    }

    /// Update the end of the selection.
    pub fn update(&mut self, point: Pos, side: Side) {
        self.region.end = Anchor::new(point, side);
    }

    pub fn is_empty(&self) -> bool {
        match self.ty {
            SelectionType::Simple => {
                let (mut start, mut end) = (self.region.start, self.region.end);

                if start.point > end.point {
                    mem::swap(&mut start, &mut end);
                }

                // Simple selection is empty when the points are identical
                // or two adjacent cells have the sides right -> left.
                start == end
                    || (start.side == Side::Right
                        && end.side == Side::Left
                        && (start.point.row == end.point.row)
                        && start.point.col + 1 == end.point.col)
            }
            SelectionType::Block => {
                let (start, end) = (self.region.start, self.region.end);

                // Block selection is empty when the points' columns and sides are identical
                // or two cells with adjacent columns have the sides right -> left,
                // regardless of their lines
                (start.point.col == end.point.col && start.side == end.side)
                    || (start.point.col + 1 == end.point.col
                        && start.side == Side::Right
                        && end.side == Side::Left)
                    || (end.point.col + 1 == start.point.col
                        && start.side == Side::Left
                        && end.side == Side::Right)
            }
            SelectionType::Semantic | SelectionType::Lines => false,
        }
    }

    /// Check whether selection contains any point in a given range.
    pub fn intersects_range<R: RangeBounds<Line>>(&self, range: R) -> bool {
        let mut start = self.region.start.point.row;
        let mut end = self.region.end.point.row;

        if start > end {
            mem::swap(&mut start, &mut end);
        }

        let range_top = match range.start_bound() {
            Bound::Included(&range_start) => range_start,
            Bound::Excluded(&range_start) => range_start + 1,
            Bound::Unbounded => Line(i32::MIN),
        };

        let range_bottom = match range.end_bound() {
            Bound::Included(&range_end) => range_end,
            Bound::Excluded(&range_end) => range_end - 1,
            Bound::Unbounded => Line(i32::MAX),
        };

        range_bottom >= start && range_top <= end
    }

    /// Convert a selection to a grid range. Anchors are SCREEN
    /// coordinates; `viewport_top` is the SCREEN row of the top visible row.
    /// Boundaries are searched on the render buffer. The result is in visible-row
    /// coordinates (the renderer feeds it with `display_offset = 0`); it may be
    /// partly off-screen, which `row_selection_for` clips per row.
    pub fn to_range_engine(
        &self,
        buf: &RenderBuffer,
        viewport_top: i32,
        escape_chars: &str,
    ) -> Option<SelectionRange> {
        let columns = buf.cols();
        let grid = VisibleGrid::new(buf.grid(), columns, buf.row_wrapped_all());

        let mut start = self.region.start;
        let mut end = self.region.end;

        if start.point > end.point {
            mem::swap(&mut start, &mut end);
        }

        start.point.row -= viewport_top;
        end.point.row -= viewport_top;

        match self.ty {
            SelectionType::Simple => self.range_simple(start, end, columns),
            SelectionType::Block => self.range_block(start, end),
            SelectionType::Lines => Some(Self::range_lines_engine(&grid, start.point, end.point)),
            SelectionType::Semantic => Some(Self::range_semantic_engine(
                &grid,
                start.point,
                end.point,
                escape_chars,
            )),
        }
    }

    /// The word or line a click at `point` selects in `grid`, the way a
    /// double or triple click does on the live screen. `None` for the kinds
    /// that do not expand from a single point.
    pub(crate) fn expand_point(
        grid: &VisibleGrid,
        point: Pos,
        ty: SelectionType,
        escape_chars: &str,
    ) -> Option<SelectionRange> {
        match ty {
            SelectionType::Semantic => Some(Self::range_semantic_engine(
                grid,
                point,
                point,
                escape_chars,
            )),
            SelectionType::Lines => Some(Self::range_lines_engine(grid, point, point)),
            SelectionType::Simple | SelectionType::Block => None,
        }
    }

    fn range_semantic_engine(
        grid: &VisibleGrid,
        mut start: Pos,
        mut end: Pos,
        escape_chars: &str,
    ) -> SelectionRange {
        if start == end
            && let Some(matching) = grid.bracket_search(start)
        {
            if (matching.row == start.row && matching.col < start.col) || (matching.row < start.row)
            {
                start = matching;
            } else {
                end = matching;
            }

            return SelectionRange {
                start,
                end,
                is_block: false,
            };
        }

        let start = grid.semantic_search_left(start, escape_chars);
        let end = grid.semantic_search_right(end, escape_chars);

        SelectionRange {
            start,
            end,
            is_block: false,
        }
    }

    fn range_lines_engine(grid: &VisibleGrid, start: Pos, end: Pos) -> SelectionRange {
        let start = grid.row_search_left(start);
        let end = grid.row_search_right(end);

        SelectionRange {
            start,
            end,
            is_block: false,
        }
    }

    fn range_simple(
        &self,
        mut start: Anchor,
        mut end: Anchor,
        columns: usize,
    ) -> Option<SelectionRange> {
        if self.is_empty() {
            return None;
        }

        // Remove last cell if selection ends to the left of a cell.
        if end.side == Side::Left && start.point != end.point {
            // Special case when selection ends to left of first cell.
            if end.point.col == 0 {
                end.point.col = Column(columns - 1);
                end.point.row -= 1;
            } else {
                end.point.col -= 1;
            }
        }

        // Remove first cell if selection starts at the right of a cell.
        if start.side == Side::Right && start.point != end.point {
            start.point.col += 1;

            // Wrap to next line when selection starts to the right of last column.
            if start.point.col == columns {
                start.point.col = Column(0);
                start.point.row += 1;
            }
        }

        Some(SelectionRange {
            start: start.point,
            end: end.point,
            is_block: false,
        })
    }

    fn range_block(&self, mut start: Anchor, mut end: Anchor) -> Option<SelectionRange> {
        if self.is_empty() {
            return None;
        }

        // Always go top-left -> bottom-right.
        if start.point.col > end.point.col {
            mem::swap(&mut start.side, &mut end.side);

            mem::swap(&mut start.point.col, &mut end.point.col);
        }

        // Remove last cell if selection ends to the left of a cell.
        if end.side == Side::Left && start.point != end.point && end.point.col.0 > 0 {
            end.point.col -= 1;
        }

        // Remove first cell if selection starts at the right of a cell.
        if start.side == Side::Right && start.point != end.point {
            start.point.col += 1;
        }

        Some(SelectionRange {
            start: start.point,
            end: end.point,
            is_block: true,
        })
    }
}

// ---------------------------------------------------------------------------
// Boundary searches over the visible grid
//
// Selection boundary searches over the render buffer's visible grid. Ports
// `Crosswords`' semantic/line/bracket searches to free
// functions over `&[Row<Square>]` + a per-row soft-wrap flag, in visible-row
// coordinates. They run against the published frame the user clicked on, so
// the result matches what was on screen even when the engine has since
// advanced; the engine's own selection operations work on its live grid.
//
// Boundaries are detected on the visible viewport; a logical line that
// soft-wraps past the viewport edge clips there to keep the scan bounded.
// ---------------------------------------------------------------------------

const BRACKET_PAIRS: [(char, char); 4] = [('(', ')'), ('[', ']'), ('{', '}'), ('<', '>')];

/// A read-only view of the render buffer's visible grid for boundary searches.
pub(crate) struct VisibleGrid<'a> {
    rows: &'a [Row<Square>],
    cols: usize,
    wrapped: &'a [bool],
}

impl<'a> VisibleGrid<'a> {
    pub(crate) fn new(rows: &'a [Row<Square>], cols: usize, wrapped: &'a [bool]) -> Self {
        Self {
            rows,
            cols,
            wrapped,
        }
    }

    pub(crate) fn last_col(&self) -> usize {
        self.cols.saturating_sub(1)
    }

    pub(crate) fn rows_len(&self) -> i32 {
        self.rows.len() as i32
    }

    pub(crate) fn cell(&self, p: Pos) -> Square {
        let y = p.row.0;

        if y < 0 {
            return Square::default();
        }

        self.rows
            .get(y as usize)
            .and_then(|r| r.inner.get(p.col.0))
            .copied()
            .unwrap_or_default()
    }

    /// Whether row `y` soft-wraps into the next row.
    pub(crate) fn wrapped(&self, y: i32) -> bool {
        if y < 0 {
            return false;
        }

        self.wrapped.get(y as usize).copied().unwrap_or(false)
    }

    /// The cell to the left, wrapping to the previous row. `None` at top-left.
    pub(crate) fn prev(&self, p: Pos) -> Option<Pos> {
        if p.col.0 > 0 {
            Some(Pos::new(p.row, Column(p.col.0 - 1)))
        } else if p.row.0 > 0 {
            Some(Pos::new(p.row - 1, Column(self.last_col())))
        } else {
            None
        }
    }

    /// The cell to the right, wrapping to the next row. `None` at bottom-right.
    pub(crate) fn next(&self, p: Pos) -> Option<Pos> {
        if p.col.0 < self.last_col() {
            Some(Pos::new(p.row, Column(p.col.0 + 1)))
        } else if p.row.0 + 1 < self.rows_len() {
            Some(Pos::new(p.row + 1, Column(0)))
        } else {
            None
        }
    }

    pub(crate) fn is_spacer(&self, p: Pos) -> bool {
        matches!(self.cell(p).wide(), Wide::Spacer | Wide::LeadingSpacer)
    }

    /// Searching left, find the next cell whose char is in `needles`. `Ok` =
    /// found, `Err` = hit a line break / grid top.
    fn inline_search_left(&self, point: Pos, needles: &str) -> Result<Pos, Pos> {
        let mut last = point;
        let mut cur = point;

        while let Some(p) = self.prev(cur) {
            // Crossed a hard line break (last column of a non-wrapped row).
            if p.col.0 == self.last_col() && !self.wrapped(p.row.0) {
                break;
            }

            last = p;

            let sq = self.cell(p);

            if !self.is_spacer(p) && needles.contains(sq.c()) {
                return Ok(p);
            }

            cur = p;
        }

        Err(last)
    }

    /// Searching right, find the next cell whose char is in `needles`.
    fn inline_search_right(&self, point: Pos, needles: &str) -> Result<Pos, Pos> {
        // Stop immediately if the start is on a hard line break.
        if point.col.0 == self.last_col() && !self.wrapped(point.row.0) {
            return Err(point);
        }

        let mut last = point;
        let mut cur = point;

        loop {
            let sq = self.cell(cur);

            if !self.is_spacer(cur) && needles.contains(sq.c()) {
                return Ok(cur);
            }

            if cur.col.0 == self.last_col() && !self.wrapped(cur.row.0) {
                break;
            }

            match self.next(cur) {
                Some(p) => {
                    last = p;
                    cur = p;
                }
                None => break,
            }
        }

        Err(last)
    }

    /// Word boundary to the left of `point` (semantic selection).
    pub(crate) fn semantic_search_left(&self, point: Pos, needles: &str) -> Pos {
        match self.inline_search_left(point, needles) {
            // Step back one cell over the escape char, skipping wide spacers.
            Ok(p) => {
                let mut q = p;

                while let Some(n) = self.next(q) {
                    q = n;

                    if !self.is_spacer(q) {
                        break;
                    }
                }

                q
            }
            Err(p) => p,
        }
    }

    /// Word boundary to the right of `point` (semantic selection).
    pub(crate) fn semantic_search_right(&self, point: Pos, needles: &str) -> Pos {
        match self.inline_search_right(point, needles) {
            Ok(p) => self.prev(p).unwrap_or(p),
            Err(p) => p,
        }
    }

    /// Start of the logical line at `point`, following soft-wrap upward.
    pub(crate) fn row_search_left(&self, mut point: Pos) -> Pos {
        while point.row.0 > 0 && self.wrapped(point.row.0 - 1) {
            point.row -= 1;
        }

        point.col = Column(0);

        point
    }

    /// End of the logical line at `point`, following soft-wrap downward.
    pub(crate) fn row_search_right(&self, mut point: Pos) -> Pos {
        while point.row.0 + 1 < self.rows_len() && self.wrapped(point.row.0) {
            point.row += 1;
        }

        point.col = Column(self.last_col());

        point
    }

    /// The matching bracket for the bracket at `point`, if any.
    pub(crate) fn bracket_search(&self, point: Pos) -> Option<Pos> {
        let start_char = self.cell(point).c();

        let (forward, end_char) = BRACKET_PAIRS.iter().find_map(|(open, close)| {
            if *open == start_char {
                Some((true, *close))
            } else if *close == start_char {
                Some((false, *open))
            } else {
                None
            }
        })?;

        let mut skip_pairs = 0i32;
        let mut cur = point;

        loop {
            let next = if forward {
                self.next(cur)
            } else {
                self.prev(cur)
            };

            let p = next?;
            let c = self.cell(p).c();

            if c == end_char && skip_pairs == 0 {
                return Some(p);
            } else if c == start_char {
                skip_pairs += 1;
            } else if c == end_char {
                skip_pairs -= 1;
            }

            cur = p;
        }
    }
}

// ---------------------------------------------------------------------------
// Per-row selection intervals for the render buffer
//
// Per-row selection interval computation for the render buffer.
//
// This is the only surviving piece of the old grid emitter. The rest — shaping
// and emitting cells into the legacy Sugarloaf terminal grid/atlas — died with
// the old renderer path; the GPUI shell paints from `RenderBuffer` directly and
// needs only this row-selection helper.
// ---------------------------------------------------------------------------

/// Per-row selection interval, in column indices. `None` = row is outside the
/// selection. Block selections reduce to the same `[lo, hi]` on every row;
/// linear selections expand middle rows to the full width.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowSelection {
    pub lo: u16,
    pub hi: u16,
}

/// Compute the selection interval (if any) for visible row `y`. The render
/// buffer is always the displayed viewport, so visible row index maps directly
/// to the absolute `Line` (no display offset).
pub fn row_selection_for(
    sel: Option<SelectionRange>,
    y: usize,
    cols: usize,
) -> Option<RowSelection> {
    let sel = sel?;

    if cols == 0 {
        return None;
    }

    let line = Line(y as i32);

    if line < sel.start.row || line > sel.end.row {
        return None;
    }

    let cols_max = cols.saturating_sub(1);

    // Block selections: every row inside the band uses the same span.
    if sel.is_block {
        let lo = sel.start.col.0.min(cols_max);
        let hi = sel.end.col.0.min(cols_max);

        return Some(RowSelection {
            lo: lo as u16,
            hi: hi as u16,
        });
    }

    let lo = if line == sel.start.row {
        sel.start.col.0
    } else {
        0
    };

    let hi = if line == sel.end.row {
        sel.end.col.0
    } else {
        cols_max
    };

    Some(RowSelection {
        lo: lo.min(cols_max) as u16,
        hi: hi.min(cols_max) as u16,
    })
}
