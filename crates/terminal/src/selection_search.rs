//! Selection boundary searches over the render buffer's visible grid. Ports
//! `Crosswords`' semantic/line/bracket searches to free
//! functions over `&[Row<Square>]` + a per-row soft-wrap flag, in visible-row
//! coordinates. The engine has no native selection-expansion, so these live here.
//!
//! Boundaries are detected on the visible viewport; a logical line that
//! soft-wraps past the viewport edge clips there to keep the scan bounded.

use crate::terminal::grid::row::Row;
use crate::terminal::pos::{Column, Line, Pos};
use crate::terminal::square::{Square, Wide};

const BRACKET_PAIRS: [(char, char); 4] = [('(', ')'), ('[', ']'), ('{', '}'), ('<', '>')];

/// A read-only view of the render buffer's visible grid for boundary searches.
pub struct VisibleGrid<'a> {
    rows: &'a [Row<Square>],
    cols: usize,
    wrapped: &'a [bool],
}

impl<'a> VisibleGrid<'a> {
    pub fn new(rows: &'a [Row<Square>], cols: usize, wrapped: &'a [bool]) -> Self {
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
    pub fn semantic_search_left(&self, point: Pos, needles: &str) -> Pos {
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
    pub fn semantic_search_right(&self, point: Pos, needles: &str) -> Pos {
        match self.inline_search_right(point, needles) {
            Ok(p) => self.prev(p).unwrap_or(p),
            Err(p) => p,
        }
    }

    /// Start of the logical line at `point`, following soft-wrap upward.
    pub fn row_search_left(&self, mut point: Pos) -> Pos {
        while point.row.0 > 0 && self.wrapped(point.row.0 - 1) {
            point.row -= 1;
        }
        point.col = Column(0);
        point
    }

    /// End of the logical line at `point`, following soft-wrap downward.
    pub fn row_search_right(&self, mut point: Pos) -> Pos {
        while point.row.0 + 1 < self.rows_len() && self.wrapped(point.row.0) {
            point.row += 1;
        }
        point.col = Column(self.last_col());
        point
    }

    /// The matching bracket for the bracket at `point`, if any.
    pub fn bracket_search(&self, point: Pos) -> Option<Pos> {
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
            let p = match next {
                Some(p) => p,
                None => return None,
            };
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

/// Convenience: a `Pos` in visible-row coordinates.
#[inline]
pub fn vpos(row: usize, col: usize) -> Pos {
    Pos::new(Line(row as i32), Column(col))
}
