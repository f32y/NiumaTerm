use crate::ghostty::GhosttyTerminal;
use crate::grid::{Column, Line, Pos, Side};
use crate::render_buffer::RenderBuffer;
use crate::selection::*;

fn buffer(cols: usize, rows: usize, bytes: &[u8]) -> RenderBuffer {
    let mut engine = GhosttyTerminal::new(cols as u16, rows as u16, 100).unwrap();

    engine.write_vt(bytes);

    let mut buf = RenderBuffer::new(cols, rows);

    engine.snapshot_into(&mut buf, 0, 0).unwrap();

    buf
}

const ESCAPE: &str = ",│`|:\"' ()[]{}<>\t\0";

#[test]
fn semantic_word_boundaries() {
    let buf = buffer(20, 1, b"foo bar baz");
    let g = VisibleGrid::new(buf.grid(), buf.cols(), buf.row_wrapped_all());

    // Click inside "bar" (col 5).
    let left = g.semantic_search_left(vpos(0, 5), ESCAPE);
    let right = g.semantic_search_right(vpos(0, 5), ESCAPE);

    assert_eq!(left.col, Column(4), "word start at 'b' of bar");
    assert_eq!(right.col, Column(6), "word end at 'r' of bar");
}

#[test]
fn bracket_match() {
    let buf = buffer(20, 1, b"a(bc)d");
    let g = VisibleGrid::new(buf.grid(), buf.cols(), buf.row_wrapped_all());

    // '(' at col 1 → matching ')' at col 4.
    assert_eq!(g.bracket_search(vpos(0, 1)), Some(vpos(0, 4)));

    // ')' at col 4 → matching '(' at col 1.
    assert_eq!(g.bracket_search(vpos(0, 4)), Some(vpos(0, 1)));

    // Non-bracket → None.
    assert_eq!(g.bracket_search(vpos(0, 0)), None);
}

#[test]
fn line_search_follows_softwrap() {
    // 13 chars on an 8-wide terminal → wraps row 0 -> row 1.
    let buf = buffer(8, 3, b"aaaaaaaaaabbb");
    let g = VisibleGrid::new(buf.grid(), buf.cols(), buf.row_wrapped_all());

    // From row 1, line start follows the wrap up to row 0 col 0.
    assert_eq!(g.row_search_left(vpos(1, 2)), vpos(0, 0));

    // From row 0, line end follows the wrap down to row 1's last column.
    assert_eq!(g.row_search_right(vpos(0, 3)), vpos(1, 7));
}

#[test]
fn line_search_stops_at_hard_newline() {
    let buf = buffer(8, 3, b"ab\r\ncd");
    let g = VisibleGrid::new(buf.grid(), buf.cols(), buf.row_wrapped_all());

    // Row 0 is hard-ended → its line is just row 0.
    assert_eq!(g.row_search_left(vpos(0, 1)), vpos(0, 0));
    assert_eq!(g.row_search_right(vpos(0, 1)).row, Line(0));
}

#[test]
fn line_search_clips_offscreen_megaline() {
    // A logical line that soft-wraps across the entire viewport (every visible
    // row wraps into the next): `row_search` clips at the visible top/bottom
    // rather than chasing the off-screen tail and turning a viewport scan unbounded.
    // 12 chars on 4 cols fill rows 0,1,2; rows 0,1 wrap, row 2 ends.
    let buf = buffer(4, 3, b"aaaaaaaaaaaa");
    let g = VisibleGrid::new(buf.grid(), buf.cols(), buf.row_wrapped_all());

    // From the middle row, the line start clips at the top visible row…
    assert_eq!(g.row_search_left(vpos(1, 2)), vpos(0, 0));

    // …and the line end clips at the bottom visible row (col = last column).
    assert_eq!(g.row_search_right(vpos(1, 2)), vpos(2, 3));
}

/// Convenience: a `Pos` in visible-row coordinates.
#[inline]
fn vpos(row: usize, col: usize) -> Pos {
    Pos::new(Line(row as i32), Column(col))
}

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

/// `to_range_engine` maps SCREEN-coord anchors to visible rows via
/// `viewport_top` and clips nothing (per-row clipping is the renderer's job).
#[test]
fn to_range_engine_maps_screen_to_visible() {
    use crate::render_buffer::RenderBuffer;

    let buf = RenderBuffer::new(10, 5);

    let mut sel = Selection::new(
        SelectionType::Simple,
        Pos::new(Line(7), Column(1)),
        Side::Left,
    );

    sel.update(Pos::new(Line(7), Column(4)), Side::Right);

    // viewport_top = 7 → screen row 7 maps to visible row 0.
    let r = sel.to_range_engine(&buf, 7, "").unwrap();

    assert_eq!(r.start.row, Line(0), "screen row - viewport_top");
    assert_eq!(r.end.row, Line(0));
}

#[test]
fn simple_is_empty() {
    let mut selection = Selection::new(
        SelectionType::Simple,
        Pos::new(Line(1), Column(0)),
        Side::Right,
    );

    assert!(selection.is_empty());

    selection.update(Pos::new(Line(1), Column(1)), Side::Left);

    assert!(selection.is_empty());

    selection.update(Pos::new(Line(0), Column(0)), Side::Right);

    assert!(!selection.is_empty());
}

#[test]
fn block_is_empty() {
    let mut selection = Selection::new(
        SelectionType::Block,
        Pos::new(Line(1), Column(0)),
        Side::Right,
    );

    assert!(selection.is_empty());

    selection.update(Pos::new(Line(1), Column(1)), Side::Left);

    assert!(selection.is_empty());

    selection.update(Pos::new(Line(1), Column(1)), Side::Right);

    assert!(!selection.is_empty());

    selection.update(Pos::new(Line(0), Column(0)), Side::Right);

    assert!(selection.is_empty());

    selection.update(Pos::new(Line(0), Column(1)), Side::Left);

    assert!(selection.is_empty());

    selection.update(Pos::new(Line(0), Column(1)), Side::Right);

    assert!(!selection.is_empty());
}

#[test]
fn range_intersection() {
    let mut selection = Selection::new(
        SelectionType::Lines,
        Pos::new(Line(3), Column(1)),
        Side::Left,
    );

    selection.update(Pos::new(Line(6), Column(1)), Side::Right);

    assert!(selection.intersects_range(..));
    assert!(selection.intersects_range(Line(2)..));
    assert!(selection.intersects_range(Line(3)..=Line(3)));
    assert!(selection.intersects_range(Line(2)..=Line(4)));
    assert!(selection.intersects_range(Line(2)..=Line(7)));
    assert!(selection.intersects_range(Line(4)..=Line(5)));
    assert!(selection.intersects_range(Line(5)..Line(8)));

    assert!(!selection.intersects_range(..=Line(2)));
    assert!(!selection.intersects_range(Line(7)..=Line(8)));
}
