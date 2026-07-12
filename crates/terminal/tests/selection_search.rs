use nmt_terminal::ghostty::GhosttyTerminal;
use nmt_terminal::render_buffer::RenderBuffer;
use nmt_terminal::selection_search::*;
use nmt_terminal::terminal::{Column, Line};

fn buffer(cols: usize, rows: usize, bytes: &[u8]) -> RenderBuffer {
    let mut engine = GhosttyTerminal::new(cols as u16, rows as u16, 100).unwrap();
    engine.write_vt(bytes);
    let mut buf = RenderBuffer::new(cols, rows);
    buf.update(&engine.snapshot().unwrap());
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
