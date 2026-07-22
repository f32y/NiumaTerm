//! Per-row selection interval computation for the render buffer.
//!
//! This is the only surviving piece of the old grid emitter. The rest — shaping
//! and emitting cells into the legacy Sugarloaf terminal grid/atlas — died with
//! the old renderer path; the GPUI shell paints from `RenderBuffer` directly and
//! needs only this row-selection helper.

use crate::selection::SelectionRange;
use crate::terminal::pos::Line;

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

#[cfg(test)]
mod tests {
    use super::{RowSelection, row_selection_for};
    use crate::selection::SelectionRange;
    use crate::terminal::pos::{Column, Line, Pos};

    fn range(sr: i32, sc: usize, er: i32, ec: usize, block: bool) -> SelectionRange {
        SelectionRange::new(
            Pos::new(Line(sr), Column(sc)),
            Pos::new(Line(er), Column(ec)),
            block,
        )
    }

    #[test]
    fn none_outside_the_row_band_or_zero_cols() {
        let sel = range(1, 2, 3, 4, false);
        assert_eq!(row_selection_for(Some(sel), 0, 10), None);
        assert_eq!(row_selection_for(Some(sel), 4, 10), None);
        assert_eq!(row_selection_for(Some(sel), 2, 0), None);
        assert_eq!(row_selection_for(None, 2, 10), None);
    }

    #[test]
    fn linear_selection_expands_middle_rows_and_clips_ends() {
        let sel = range(1, 2, 3, 4, false);
        // First row: starts at start col, runs to the edge.
        assert_eq!(
            row_selection_for(Some(sel), 1, 10),
            Some(RowSelection { lo: 2, hi: 9 })
        );
        // Middle row: full width.
        assert_eq!(
            row_selection_for(Some(sel), 2, 10),
            Some(RowSelection { lo: 0, hi: 9 })
        );
        // Last row: from the edge to end col.
        assert_eq!(
            row_selection_for(Some(sel), 3, 10),
            Some(RowSelection { lo: 0, hi: 4 })
        );
    }

    #[test]
    fn block_selection_uses_the_same_span_on_every_row() {
        let sel = range(1, 2, 3, 4, true);
        let expected = Some(RowSelection { lo: 2, hi: 4 });
        assert_eq!(row_selection_for(Some(sel), 1, 10), expected);
        assert_eq!(row_selection_for(Some(sel), 2, 10), expected);
        assert_eq!(row_selection_for(Some(sel), 3, 10), expected);
    }
}
