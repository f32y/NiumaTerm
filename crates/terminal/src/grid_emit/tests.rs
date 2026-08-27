use crate::grid_emit::{RowSelection, row_selection_for};
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
