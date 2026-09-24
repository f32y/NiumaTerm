use crate::ghostty::GhosttyTerminal;
use crate::grid::{Column, Line, Pos, Side};
use crate::render_buffer::RenderBuffer;
use crate::selection::{Selection, SelectionType};
use crate::session::selection::{SurfaceSelection, block_selection_range, selection_screen_range};
use crate::session::{SurfaceCellSide, SurfaceMouseEventKind, paste_payload};

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

// ---------------------------------------------------------------------------
// Screen-range and block-range resolution, mouse report encoding
// ---------------------------------------------------------------------------

#[test]
fn frozen_click_selection_expands_words_and_wrapped_lines() {
    let mut terminal = GhosttyTerminal::new(8, 4, 100).unwrap();

    terminal.write_vt(b"foo bar\r\nabcdefghijk");

    let handle = terminal.finish_block().unwrap().expect("block created");
    let block = terminal.block_acquire(handle).expect("block acquired");
    let palette = terminal.color_palette();

    assert_eq!(
        block_selection_range(&block, &palette, 0, 5, SelectionType::Semantic),
        Some(((0, 4), (0, 6)))
    );
    assert_eq!(
        block_selection_range(&block, &palette, 2, 1, SelectionType::Semantic),
        Some(((1, 0), (2, 2)))
    );
    assert_eq!(
        block_selection_range(&block, &palette, 2, 1, SelectionType::Lines),
        Some(((1, 0), (2, 7)))
    );
}

#[test]
fn screen_word_selection_searches_the_visible_row_before_rebasing() {
    let mut terminal = GhosttyTerminal::new(24, 2, 100).unwrap();

    terminal.write_vt(b"pipelines.universal\r\npi");

    let mut buf = RenderBuffer::new(24, 2);

    terminal.snapshot_into(&mut buf, 0, 0).unwrap();

    let selection = Selection::new(
        SelectionType::Semantic,
        Pos::new(Line(1), Column(1)),
        Side::Left,
    );

    let range = selection_screen_range(&selection, &buf, 1).unwrap();

    assert_eq!(range.start, Pos::new(Line(1), Column(0)));
    assert_eq!(range.end, Pos::new(Line(1), Column(8)));
}

#[test]
fn paste_payload_normalizes_and_guards() {
    assert_eq!(paste_payload("", false), None);
    assert_eq!(
        paste_payload("a\r\nb\nc", false).unwrap(),
        b"a\rb\rc".to_vec()
    );
    assert_eq!(
        paste_payload("ls\x1b[201~rm", true).unwrap(),
        b"\x1b[200~lsrm\x1b[201~".to_vec()
    );
}

/// Frozen history expands a double click the way the live screen does,
/// including jumping from a bracket to its match.
#[test]
fn frozen_click_selection_matches_brackets_like_the_live_screen() {
    let mut terminal = GhosttyTerminal::new(20, 4, 100).unwrap();

    terminal.write_vt(b"f(a, b) x");

    let handle = terminal.finish_block().unwrap().expect("block created");
    let block = terminal.block_acquire(handle).expect("block acquired");
    let palette = terminal.color_palette();

    assert_eq!(
        block_selection_range(&block, &palette, 0, 1, SelectionType::Semantic),
        Some(((0, 1), (0, 6)))
    );
}
