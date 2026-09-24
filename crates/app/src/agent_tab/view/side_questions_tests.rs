use gpui::{Bounds, Pixels, Size, point, px, size};

use crate::agent_tab::view::side_questions::{Edges, Gesture, dragged, fit};

fn start() -> Bounds<Pixels> {
    Bounds::new(point(px(100.), px(100.)), size(px(400.), px(300.)))
}

const PANE: Size<Pixels> = size(px(1000.), px(800.));

fn resize(left: bool, right: bool, top: bool, bottom: bool) -> Gesture {
    Gesture::Resize(Edges {
        left,
        right,
        top,
        bottom,
    })
}

#[test]
fn a_left_edge_resize_keeps_the_right_edge_in_place() {
    let bounds = dragged(
        start(),
        resize(true, false, false, false),
        point(px(-50.), px(20.)),
        PANE,
    );

    assert_eq!(
        bounds,
        Bounds::new(point(px(50.), px(100.)), size(px(450.), px(300.)))
    );
}

#[test]
fn a_corner_resize_moves_both_edges_it_joins() {
    let bounds = dragged(
        start(),
        resize(false, true, false, true),
        point(px(30.), px(40.)),
        PANE,
    );

    assert_eq!(
        bounds,
        Bounds::new(point(px(100.), px(100.)), size(px(430.), px(340.)))
    );
}

#[test]
fn resizing_past_the_minimum_pins_the_opposite_edge() {
    let bounds = dragged(
        start(),
        resize(true, false, true, false),
        point(px(1000.), px(1000.)),
        PANE,
    );

    // The right and bottom edges stay where they were; the card neither
    // flips nor slides.
    assert_eq!(bounds.right(), px(500.));
    assert_eq!(bounds.bottom(), px(400.));
    assert_eq!(bounds.size, size(px(280.), px(180.)));
}

#[test]
fn resizing_stops_at_the_pane_edges() {
    let grown = dragged(
        start(),
        resize(true, true, true, true),
        point(px(-500.), px(-500.)),
        PANE,
    );

    assert_eq!(grown.origin, point(px(0.), px(0.)));

    let grown = dragged(
        start(),
        resize(false, true, false, true),
        point(px(5000.), px(5000.)),
        PANE,
    );

    assert_eq!(grown.right(), PANE.width);
    assert_eq!(grown.bottom(), PANE.height);
}

#[test]
fn a_move_keeps_the_size_and_stays_inside_the_pane() {
    let moved = dragged(start(), Gesture::Move, point(px(900.), px(-900.)), PANE);

    assert_eq!(
        moved,
        Bounds::new(point(px(600.), px(0.)), size(px(400.), px(300.)))
    );
}

#[test]
fn a_shrinking_pane_shrinks_the_card_and_pins_it_top_left() {
    let fitted = fit(start(), size(px(300.), px(200.)));

    assert_eq!(
        fitted,
        Bounds::new(point(px(0.), px(0.)), size(px(300.), px(200.)))
    );

    // An unmeasured pane leaves the card alone rather than collapsing it.
    assert_eq!(fit(start(), size(px(0.), px(0.))), start());
}
