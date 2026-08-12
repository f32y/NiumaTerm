use gpui::{ScrollDelta, point, px};
use nmt_terminal::selection::SelectionType;

use crate::terminal::block_list::{BlockListState, block_list_alignment};
use crate::terminal::surface::{SurfaceCell, SurfaceCellSide};
use crate::terminal::view::scroll::{scroll_block_list_to_latest, viewport_is_scrolled};
use crate::terminal::view::{
    dropped_paths_text, metrics, selection_drag_started, selection_type_for_click_count,
    terminal_cell_at_position, terminal_scroll_lines,
};

#[test]
fn repeated_clicks_choose_terminal_selection_modes() {
    assert_eq!(selection_type_for_click_count(1), SelectionType::Simple);
    assert_eq!(selection_type_for_click_count(2), SelectionType::Semantic);
    assert_eq!(selection_type_for_click_count(3), SelectionType::Lines);
    assert_eq!(selection_type_for_click_count(4), SelectionType::Lines);
}

#[test]
fn dropped_paths_are_space_delimited_and_paths_with_spaces_are_quoted() {
    assert_eq!(
        dropped_paths_text(&[
            "C:\\src\\main.rs".into(),
            "C:\\My Project\\notes.txt".into(),
        ]),
        "C:\\src\\main.rs \"C:\\My Project\\notes.txt\""
    );
}

#[test]
fn block_gutter_hit_band() {
    use crate::terminal::view::block_gutter_hit;
    let origin_x = 10.0;
    assert!(block_gutter_hit(10.0 - 5.0, origin_x), "on the strip");
    assert!(
        block_gutter_hit(10.0 + 2.0, origin_x),
        "tolerance into col 0"
    );
    assert!(
        !block_gutter_hit(10.0 + 6.0, origin_x),
        "column 0 text is not the gutter"
    );
    assert!(
        block_gutter_hit(0.0, origin_x),
        "strip is flush with the pane edge"
    );
    assert!(!block_gutter_hit(-3.0, origin_x), "left of the pane misses");
}

#[test]
fn mouse_position_maps_to_cell_and_side() {
    let cell = metrics::CellMetrics {
        width_px: 8.0,
        height_px: 18.0,
    };

    // Content origin at (10, 10): position 26,46 -> local 16,36 -> col 2 row 2.
    let origin = point(px(10.0), px(10.0));
    assert_eq!(
        terminal_cell_at_position(point(px(26.0), px(46.0)), origin, cell, &[]),
        (SurfaceCell { col: 2, row: 2 }, SurfaceCellSide::Left)
    );
    assert_eq!(
        terminal_cell_at_position(point(px(31.0), px(46.0)), origin, cell, &[]),
        (SurfaceCell { col: 2, row: 2 }, SurfaceCellSide::Right)
    );
}

#[test]
fn selection_drag_waits_for_quarter_cell_movement() {
    let origin = point(px(10.0), px(10.0));

    assert!(!selection_drag_started(
        origin,
        point(px(11.0), px(11.0)),
        8.0
    ));
    assert!(selection_drag_started(
        origin,
        point(px(12.0), px(10.0)),
        8.0
    ));
}

#[test]
fn scroll_delta_maps_to_terminal_lines() {
    let cell = metrics::CellMetrics {
        width_px: 8.0,
        height_px: 20.0,
    };

    assert_eq!(
        terminal_scroll_lines(ScrollDelta::Pixels(point(px(0.0), px(60.0))), cell),
        3
    );
    assert_eq!(
        terminal_scroll_lines(ScrollDelta::Lines(point(0.0, -2.0)), cell),
        -6
    );
    assert_eq!(
        terminal_scroll_lines(ScrollDelta::Pixels(point(px(0.0), px(4.0))), cell),
        0
    );
}

#[test]
fn typed_input_can_restore_a_scrolled_block_list() {
    let mut block_list = BlockListState::new(block_list_alignment(false));
    block_list.scrollbar = (24.0, 120.0);

    assert!(scroll_block_list_to_latest(&mut block_list));
    assert_eq!(block_list.scrollbar, (120.0, 120.0));
    assert_eq!(
        block_list.list.logical_scroll_top().item_ix,
        block_list.list.item_count()
    );

    assert!(!scroll_block_list_to_latest(&mut block_list));
}

#[test]
fn terminal_viewport_only_moves_when_above_the_bottom() {
    assert!(viewport_is_scrolled(3, 20, 10));
    assert!(!viewport_is_scrolled(10, 20, 10));
    assert!(!viewport_is_scrolled(0, 10, 20));
}
