use nmt_terminal::ghostty::GhosttyTerminal;
use nmt_terminal::render_buffer::RenderBuffer;
use nmt_terminal::selection_search::{VisibleGrid, vpos};
use nmt_terminal::terminal::{Line, Pos};
use nmt_terminal::vi_motion::*;

const ESCAPE: &str = ",│`|:\"' ()[]{}<>\t\0";

fn grid_of(cols: usize, rows: usize, bytes: &[u8]) -> RenderBuffer {
    let mut engine = GhosttyTerminal::new(cols as u16, rows as u16, 100).unwrap();
    engine.write_vt(bytes);
    let mut buf = RenderBuffer::new(cols, rows);
    engine.snapshot_into(&mut buf).unwrap();
    buf
}

fn motion(buf: &RenderBuffer, pos: Pos, m: ViMotion) -> Pos {
    let g = VisibleGrid::new(buf.grid(), buf.cols(), buf.row_wrapped_all());
    vi_motion(&g, ESCAPE, pos, m)
}

#[test]
fn motion_simple() {
    let buf = grid_of(20, 20, b"");
    let mut p = vpos(0, 0);
    p = motion(&buf, p, ViMotion::Right);
    assert_eq!(p, vpos(0, 1));
    p = motion(&buf, p, ViMotion::Left);
    assert_eq!(p, vpos(0, 0));
    p = motion(&buf, p, ViMotion::Down);
    assert_eq!(p, vpos(1, 0));
    p = motion(&buf, p, ViMotion::Up);
    assert_eq!(p, vpos(0, 0));
}

#[test]
fn motion_up_clamps_at_top() {
    let buf = grid_of(20, 20, b"");
    // At the top visible row, Up clamps (the handler scrolls past the edge).
    assert_eq!(motion(&buf, vpos(0, 3), ViMotion::Up), vpos(0, 3));
}

#[test]
fn motion_start_end_blank() {
    // Blank row: Last → the last column, First → column 0.
    let buf = grid_of(20, 20, b"");
    let mut p = vpos(0, 0);
    p = motion(&buf, p, ViMotion::Last);
    assert_eq!(p, vpos(0, 19));
    p = motion(&buf, p, ViMotion::First);
    assert_eq!(p, vpos(0, 0));
}

#[test]
fn motion_last_jumps_to_last_occupied() {
    // "hello": Last from col 0 jumps to the last occupied cell ('o', col 4).
    let buf = grid_of(20, 20, b"hello");
    assert_eq!(motion(&buf, vpos(0, 0), ViMotion::Last), vpos(0, 4));
}

#[test]
fn motion_bracket() {
    let buf = grid_of(20, 3, b"(x)");
    let mut p = vpos(0, 0);
    p = motion(&buf, p, ViMotion::Bracket);
    assert_eq!(p, vpos(0, 2));
    p = motion(&buf, p, ViMotion::Bracket);
    assert_eq!(p, vpos(0, 0));
}

#[test]
fn motion_word_right() {
    let buf = grid_of(20, 3, b"foo bar baz");
    // From start of "foo", WordRight lands at the start of "bar".
    let p = motion(&buf, vpos(0, 0), ViMotion::WordRight);
    assert_eq!(p, vpos(0, 4));
}

#[test]
fn motion_high_middle_low() {
    let buf = grid_of(20, 20, b"");
    assert_eq!(motion(&buf, vpos(5, 0), ViMotion::High).row, Line(0));
    assert_eq!(motion(&buf, vpos(5, 0), ViMotion::Middle).row, Line(9));
    assert_eq!(motion(&buf, vpos(5, 0), ViMotion::Low).row, Line(19));
}

/// Moving left with `b` from the start of a later word lands on the previous
/// word start.
#[test]
fn motion_word_left() {
    let buf = grid_of(20, 3, b"foo bar baz");
    // From start of "baz" (col 8), WordLeft lands at the start of "bar".
    let p = motion(&buf, vpos(0, 8), ViMotion::WordLeft);
    assert_eq!(p, vpos(0, 4));
}

/// `^` (FirstOccupied) lands on the first non-blank cell.
#[test]
fn motion_first_occupied() {
    let buf = grid_of(20, 3, b"  hi");
    let p = motion(&buf, vpos(0, 3), ViMotion::FirstOccupied);
    assert_eq!(p, vpos(0, 2));
}

#[test]
fn wide_char_skips_spacer() {
    // "a汉a": col0 'a', col1 wide '汉', col2 spacer, col3 'a'.
    let buf = grid_of(20, 3, "a汉a".as_bytes());
    // Right from the wide cell jumps past its spacer.
    let p = motion(&buf, vpos(0, 1), ViMotion::Right);
    assert_eq!(p, vpos(0, 3));
}
