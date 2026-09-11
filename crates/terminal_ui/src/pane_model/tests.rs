use nmt_input::keyboard::ModifiersState;
use nmt_terminal::block_store::BlockStore;
use nmt_terminal::ghostty::ScrollbarInfo;
use nmt_terminal::selection::SelectionType;
use nmt_terminal::session::{SurfaceCell, SurfaceCellSide, SurfaceMouseButton};

use crate::block_list::reconcile::BlockListRenderMetrics;
use crate::metrics::CellMetrics;
use crate::pane_model::key_action::{KeyOutcome, TerminalKeyAction};
use crate::pane_model::list_mirror::{BlockListMirror, ListOp, ListPosition};
use crate::pane_model::mouse::{MouseInput, MouseOutcome};
use crate::pane_model::scroll::ScrollOutcome;
use crate::pane_model::selection_drag::{
    block_gutter_hit, selection_drag_started, selection_type_for_click_count,
};
use crate::pane_model::test_session::controller;
use crate::pane_model::viewport::{LocalPoint, Viewport};

#[test]
fn repeated_clicks_choose_terminal_selection_modes() {
    assert_eq!(selection_type_for_click_count(1), SelectionType::Simple);
    assert_eq!(selection_type_for_click_count(2), SelectionType::Semantic);
    assert_eq!(selection_type_for_click_count(3), SelectionType::Lines);
    assert_eq!(selection_type_for_click_count(4), SelectionType::Lines);
}
#[test]
fn block_gutter_hit_band() {
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
fn selection_drag_waits_for_quarter_cell_movement() {
    let origin = LocalPoint { x: 10.0, y: 10.0 };

    assert!(!selection_drag_started(
        origin,
        LocalPoint { x: 11.0, y: 11.0 },
        8.0
    ));
    assert!(selection_drag_started(
        origin,
        LocalPoint { x: 12.0, y: 10.0 },
        8.0
    ));
}

#[test]
fn both_viewports_map_pointer_cursor_and_thumb_consistently() {
    let cell = CellMetrics {
        width_px: 8.0,
        height_px: 18.0,
    };
    for viewport in [
        Viewport::Grid {
            scrollbar: ScrollbarInfo {
                total: 40,
                offset: 10,
                len: 20,
            },
            row_offsets: vec![36.0; 4],
        },
        Viewport::BlockList {
            scroll_px: 10.0,
            max_scroll_px: 20.0,
            active_top: 36.0,
            viewport_px: 20.0,
        },
    ] {
        assert!(viewport.is_scrolled());
        assert_eq!(
            viewport.cell_at(LocalPoint { x: 16.0, y: 72.0 }, cell),
            (SurfaceCell { col: 2, row: 2 }, SurfaceCellSide::Left)
        );
        assert_eq!(
            viewport.cell_at(LocalPoint { x: 23.0, y: 72.0 }, cell).1,
            SurfaceCellSide::Right
        );
        assert_eq!(viewport.cursor_y(2, cell.height_px), 72.0);
        assert_eq!(viewport.thumb_target(0.5), Some(20.0));
    }
}

fn left_press(position: LocalPoint) -> MouseInput {
    MouseInput {
        position,
        button: Some(SurfaceMouseButton::Left),
        modifiers: ModifiersState::empty(),
        click_count: 1,
        follow_link: false,
    }
}

#[test]
fn frozen_selection_obeys_mouse_reporting_and_drag_threshold() {
    for reporting in [false, true] {
        let (mut model, _) = controller(
            if reporting {
                b"\x1b[?1000h\x1b[?1006h"
            } else {
                b""
            },
            true,
        );
        model.frozen.begin_frame(54.0);
        model.frozen.push_row(0.0, 0, 0, 40);
        model.frozen.push_row(18.0, 0, 1, 40);
        model.update_viewport();
        assert!(matches!(
            (
                reporting,
                model.mouse_down(left_press(LocalPoint { x: 16.0, y: 4.0 }))
            ),
            (true, MouseOutcome::EngineHandled) | (false, MouseOutcome::FrozenSelectionStarted)
        ));
        assert_eq!(model.frozen_drag.anchor().is_some(), !reporting);
        if !reporting {
            assert!(matches!(
                model.mouse_move(left_press(LocalPoint { x: 17.0, y: 4.0 })),
                MouseOutcome::Ignored
            ));
            assert!(model.frozen_drag.current().is_none());
            assert!(matches!(
                model.mouse_move(left_press(LocalPoint { x: 40.0, y: 20.0 })),
                MouseOutcome::SelectionChanged
            ));
            let (a, b) = model.frozen_drag.current().unwrap();
            assert_eq!((a.line, a.col, b.line, b.col), (0, 2, 1, 5));
            model.mouse_move(left_press(LocalPoint { x: 48.0, y: 120.0 }));
            assert_eq!(model.frozen_drag.current().unwrap().1.line, 1);
            model.mouse_up(left_press(LocalPoint { x: 40.0, y: 20.0 }));
            assert!(model.frozen_drag.anchor().is_none());
            assert!(model.frozen_drag.current().is_some());
        }
    }
}

#[test]
fn modified_link_click_precedes_program_mouse_reporting() {
    let (mut model, _) = controller(b"https://example.com\x1b[?1000h", false);
    let mut input = left_press(LocalPoint { x: 40.0, y: 4.0 });
    input.follow_link = true;
    assert!(
        matches!(model.mouse_down(input), MouseOutcome::OpenUrl(url) if url == "https://example.com")
    );
    assert!(model.source.session.selection_range().is_none());
    assert!(model.frozen_drag.anchor().is_none());
}

#[test]
fn key_outcomes_distinguish_accepted_input_from_read_only_rejection() {
    let (mut model, _) = controller(b"", true);
    model.block_list.scrollbar = (24.0, 120.0);
    model.update_viewport();
    assert!(matches!(
        model.apply_key_action(TerminalKeyAction::Write(vec![0x1b])),
        KeyOutcome::Written
    ));
    assert!(
        model.viewport.is_scrolled(),
        "the host chooses when accepted input scrolls the view"
    );
    assert!(matches!(
        model.scroll_to_latest(),
        ScrollOutcome::List(ListOp::ScrollToEnd)
    ));
    assert!(!model.viewport.is_scrolled());
    assert!(matches!(model.scroll_to_latest(), ScrollOutcome::Ignored));
    model.source.session.mark_read_only();
    assert!(matches!(
        model.apply_key_action(TerminalKeyAction::Write(vec![0x1b])),
        KeyOutcome::Ignored
    ));
}

#[test]
fn list_mirror_plans_growth_eviction_remeasurement_and_scroll() {
    let mut mirror = BlockListMirror::default();
    let mut metrics = BlockListRenderMetrics {
        store_len: 2,
        evicted_items: 0,
        item_count: 3,
        frozen_px: 60.0,
        tail_px: 0.0,
        total_px: 100.0,
        offset_px: 0.0,
        last_item_px: 30.0,
    };
    let layout = (40, 18.0, 1.0);
    assert_eq!(
        mirror.sync(&metrics, layout, 2),
        [ListOp::Splice(0..1, 3), ListOp::Remeasure(1..3)]
    );
    assert!(mirror.sync(&metrics, layout, 2).is_empty());
    assert_eq!(
        mirror.sync(&metrics, (40, 18.0, 0.0), 2),
        [ListOp::RemeasureAll]
    );
    metrics.evicted_items = 1;
    metrics.store_len = 1;
    metrics.item_count = 2;
    assert_eq!(
        mirror.sync(&metrics, (40, 18.0, 0.0), 2),
        [ListOp::Splice(0..1, 0), ListOp::Remeasure(0..2)]
    );
    let store = BlockStore::default();
    assert_eq!(
        BlockListMirror::scroll_to_px(&store, 0, 2, layout, 10.0),
        ListOp::ScrollTo(ListPosition {
            item_ix: 0,
            offset_px: 10.0
        })
    );
    assert_eq!(
        BlockListMirror::scroll_to_px(&store, 0, 2, layout, 100.0),
        ListOp::ScrollToEnd
    );
}

#[test]
fn presentation_modules_do_not_import_host_services() {
    use std::fs;
    use std::path::Path;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for directory in ["pane_model", "block_list", "frame", "links"] {
        for entry in fs::read_dir(root.join(directory)).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy();
            if !name.ends_with(".rs") || name.contains("test") || name.contains("profile") {
                continue;
            }
            let text = fs::read_to_string(&path).unwrap();
            for line in text.lines() {
                let line = line.trim();
                if line == "use gpui::SharedString;" && directory == "frame" && name == "line.rs" {
                    continue;
                }
                assert!(
                    !line.contains("use gpui")
                        && !line.contains("crate::view")
                        && !line.contains("crate::paint")
                        && !line.contains("use nmt_i18n")
                        && !line.contains("active_colors"),
                    "host dependency in {}: {line}",
                    path.display()
                );
            }
        }
    }
}
