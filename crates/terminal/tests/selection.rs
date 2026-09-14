//! Selection tests.

use nmt_terminal::selection::*;
use nmt_terminal::terminal::pos::{Column, Line, Pos, Side};

/// `to_range_engine` maps SCREEN-coord anchors to visible rows via
/// `viewport_top` and clips nothing (per-row clipping is the renderer's job).
#[test]
fn to_range_engine_maps_screen_to_visible() {
    use nmt_terminal::render_buffer::RenderBuffer;

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
