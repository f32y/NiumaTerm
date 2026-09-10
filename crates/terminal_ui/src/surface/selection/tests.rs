use nmt_terminal::selection::SelectionType;
use nmt_terminal::terminal::pos::{Column, Line, Pos};

use crate::surface::mouse::{SurfaceCellSide, SurfaceMouseEventKind};
use crate::surface::selection::SurfaceSelection;

fn pos(row: i32, col: usize) -> Pos {
    Pos::new(Line(row), Column(col))
}

/// A plain click starts an empty selection, so nothing on screen changed yet
/// and the caller must not be told to repaint.
#[test]
fn plain_click_on_empty_selection_needs_no_repaint() {
    let selection = SurfaceSelection::default();

    assert!(!selection.apply_at(
        pos(3, 4),
        SurfaceCellSide::Left,
        SurfaceMouseEventKind::Down,
        SelectionType::Simple,
    ));
}

/// A click while a highlight is showing replaces it, so the old highlight has
/// to be repainted away even though the new selection is still empty.
#[test]
fn click_replacing_a_highlight_needs_a_repaint() {
    let selection = SurfaceSelection::default();

    selection.apply_at(
        pos(3, 4),
        SurfaceCellSide::Left,
        SurfaceMouseEventKind::Down,
        SelectionType::Simple,
    );
    selection.apply_at(
        pos(3, 9),
        SurfaceCellSide::Right,
        SurfaceMouseEventKind::Move,
        SelectionType::Simple,
    );

    assert!(selection.apply_at(
        pos(5, 0),
        SurfaceCellSide::Left,
        SurfaceMouseEventKind::Down,
        SelectionType::Simple,
    ));
}

/// Word and line selections cover cells the moment the button goes down, so
/// they always need a repaint.
#[test]
fn semantic_click_needs_a_repaint_immediately() {
    let selection = SurfaceSelection::default();

    assert!(selection.apply_at(
        pos(3, 4),
        SurfaceCellSide::Left,
        SurfaceMouseEventKind::Down,
        SelectionType::Semantic,
    ));
}

/// Motion without a button press must not resurrect a selection; the terminal
/// receives plain hover events whenever the pointer crosses the grid.
#[test]
fn drag_without_an_active_selection_is_ignored() {
    let selection = SurfaceSelection::default();

    assert!(!selection.apply_at(
        pos(3, 4),
        SurfaceCellSide::Left,
        SurfaceMouseEventKind::Move,
        SelectionType::Simple,
    ));
}

/// Releasing a click that never moved drops the empty selection, so a later
/// stray motion has nothing to extend.
#[test]
fn releasing_an_empty_click_drops_the_selection() {
    let selection = SurfaceSelection::default();

    selection.apply_at(
        pos(3, 4),
        SurfaceCellSide::Left,
        SurfaceMouseEventKind::Down,
        SelectionType::Simple,
    );
    selection.apply_at(
        pos(3, 4),
        SurfaceCellSide::Left,
        SurfaceMouseEventKind::Up,
        SelectionType::Simple,
    );

    assert!(!selection.apply_at(
        pos(3, 9),
        SurfaceCellSide::Right,
        SurfaceMouseEventKind::Move,
        SelectionType::Simple,
    ));
}

/// Clearing leaves no anchor behind, so the next click is treated as the first
/// one again.
#[test]
fn clearing_removes_the_anchor() {
    let selection = SurfaceSelection::default();

    selection.apply_at(
        pos(3, 4),
        SurfaceCellSide::Left,
        SurfaceMouseEventKind::Down,
        SelectionType::Simple,
    );
    selection.clear();

    assert!(!selection.apply_at(
        pos(7, 1),
        SurfaceCellSide::Left,
        SurfaceMouseEventKind::Down,
        SelectionType::Simple,
    ));
}
