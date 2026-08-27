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
mod tests;
