// Retired from: https://github.com/alacritty/alacritty/blob/6e7f466c68b387f41726757eed4f3e70d05479d2/alacritty_terminal/src/selection.rs
// which is licensed under Apache 2.0 license.
//! State management for a selection in the grid.
//!
//! A selection should start when the mouse is clicked, and it should be
//! finalized when the button is released. The selection should be cleared
//! when text is added/removed/scrolled on the screen. The selection should
//! also be cleared if the user clicks off of the selection.

use std::cmp::min;
use std::mem;
use std::ops::{self, Bound, Range, RangeBounds};

use crate::render_buffer::RenderBuffer;
use crate::selection_search::VisibleGrid;
use crate::terminal::grid::Dimensions;
use crate::terminal::pos::{Column, Line, Pos, Side};

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

impl SelectionRange {
    /// The visible row indices this selection damages, clamped to a `rows`-tall
    /// viewport, or `None` when the selection is fully outside it.
    /// Mirrors the retired `TermDamageState::damage_selection` at
    /// `display_offset == 0` — the render buffer is always the displayed viewport
    /// without scanning unrelated rows.
    /// `start <= end` is guaranteed by `SelectionRange::new`.
    pub fn visible_rows_clamped(&self, rows: usize) -> Option<ops::RangeInclusive<usize>> {
        if rows == 0 {
            return None;
        }
        let last = rows as i32 - 1;
        let (start_row, end_row) = (self.start.row.0, self.end.row.0);
        // Fully above (end before row 0) or below (start past the last row).
        if end_row < 0 || start_row > last {
            return None;
        }
        let start = start_row.max(0) as usize;
        let end = end_row.clamp(0, last) as usize;
        Some(start..=end)
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

    pub fn rotate<D: Dimensions>(
        mut self,
        dimensions: &D,
        range: &Range<Line>,
        delta: i32,
    ) -> Option<Selection> {
        let bottommost_line = dimensions.bottommost_line();
        let range_bottom = range.end;
        let range_top = range.start;

        let (mut start, mut end) = (&mut self.region.start, &mut self.region.end);
        if start.point > end.point {
            mem::swap(&mut start, &mut end);
        }

        // Rotate start of selection.
        if (start.point.row >= range_top || range_top == 0) && start.point.row < range_bottom {
            start.point.row = min(start.point.row - delta, bottommost_line);

            // If end is within the same region, delete selection once start rotates out.
            if start.point.row >= range_bottom && end.point.row < range_bottom {
                return None;
            }

            // Clamp selection to start of region.
            if start.point.row < range_top && range_top != 0 {
                if self.ty != SelectionType::Block {
                    start.point.col = Column(0);
                    start.side = Side::Left;
                }
                start.point.row = range_top;
            }
        }

        // Rotate end of selection.
        if (end.point.row >= range_top || range_top == 0) && end.point.row < range_bottom {
            end.point.row = min(end.point.row - delta, bottommost_line);

            // Delete selection if end has overtaken the start.
            if end.point.row < start.point.row {
                return None;
            }

            // Clamp selection to end of region.
            if end.point.row >= range_bottom {
                if self.ty != SelectionType::Block {
                    end.point.col = dimensions.last_column();
                    end.side = Side::Right;
                }
                end.point.row = range_bottom - 1;
            }
        }

        Some(self)
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

    fn range_semantic_engine(
        grid: &VisibleGrid,
        mut start: Pos,
        mut end: Pos,
        escape_chars: &str,
    ) -> SelectionRange {
        if start == end {
            if let Some(matching) = grid.bracket_search(start) {
                if (matching.row == start.row && matching.col < start.col)
                    || (matching.row < start.row)
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
