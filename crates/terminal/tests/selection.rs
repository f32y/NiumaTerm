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

/// `visible_rows_clamped` reproduces the retired
/// `TermDamageState::damage_selection` clamp (display_offset == 0).
#[test]
fn visible_rows_clamped_cases() {
    let mk = |s: i32, e: i32| {
        SelectionRange::new(
            Pos::new(Line(s), Column(0)),
            Pos::new(Line(e), Column(0)),
            false,
        )
    };

    // Fully inside.
    assert_eq!(mk(1, 3).visible_rows_clamped(5), Some(1..=3));
    // Single row.
    assert_eq!(mk(2, 2).visible_rows_clamped(5), Some(2..=2));
    // Spans past the bottom → clamps end to last row.
    assert_eq!(mk(3, 9).visible_rows_clamped(5), Some(3..=4));
    // Starts above row 0 → clamps start to 0.
    assert_eq!(mk(-2, 1).visible_rows_clamped(5), Some(0..=1));
    // Spans the whole (and beyond) viewport.
    assert_eq!(mk(-5, 99).visible_rows_clamped(5), Some(0..=4));
    // Fully above the viewport → None.
    assert_eq!(mk(-4, -1).visible_rows_clamped(5), None);
    // Fully below the viewport → None.
    assert_eq!(mk(5, 8).visible_rows_clamped(5), None);
    // Zero-height viewport → None (no panic).
    assert_eq!(mk(0, 0).visible_rows_clamped(0), None);
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
