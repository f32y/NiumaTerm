use gpui::{ListAlignment, ListOffset, px};
use nmt_terminal::event::BlockEvent;
use nmt_terminal::ghostty::BlockHandle;

use crate::{block_list, theme};

fn block_item(seq: u64, id: u64, rows: usize) -> BlockEvent {
    BlockEvent::EngineBlock {
        seq,
        handle: BlockHandle { id, generation: 1 },
        rows,
    }
}

#[test]
fn block_list_active_top_survives_when_live_item_is_not_prepainted() {
    use nmt_terminal::block_store::BlockStore;

    let mut store = BlockStore::default();
    store.apply([block_item(1, 1, 1)]);
    // One 1-row item = 1 content row + 2 pad rows = 30px; the live grid
    // then starts after its own top pad (10px), minus the 5px scroll.
    let pad = block_list::ITEM_PAD_ROWS;
    let frozen_px: f32 = store
        .items()
        .iter()
        .map(|item| block_list::item_px(item, 80, 10.0, pad))
        .sum();
    assert_eq!(
        block_list::block_list_active_top_px(frozen_px, 0.0, 10.0, pad, 5.0),
        35.0
    );
    // Compact presentation: no pads anywhere, so the live grid starts
    // right after the frozen rows.
    let compact_px: f32 = store
        .items()
        .iter()
        .map(|item| block_list::item_px(item, 80, 10.0, 0.0))
        .sum();
    assert_eq!(compact_px, 10.0, "1 content row, no pad rows");
    assert_eq!(
        block_list::block_list_active_top_px(compact_px, 0.0, 10.0, 0.0, 5.0),
        5.0
    );
}

#[test]
fn block_list_render_metrics_resolve_scroll_once() {
    use nmt_terminal::block_store::BlockStore;

    let mut store = BlockStore::default();
    store.apply([block_item(1, 1, 1)]);

    let metrics = block_list::block_list_render_metrics(
        &store,
        2,
        1,
        80,
        10.0,
        block_list::ITEM_PAD_ROWS,
        ListOffset {
            item_ix: 1,
            offset_in_item: px(3.0),
        },
    );

    assert_eq!(metrics.store_len, 1);
    assert_eq!(metrics.item_count, 2);
    assert_eq!(metrics.frozen_px, 30.0, "1 row + 2 pad rows");
    assert_eq!(metrics.tail_px, 10.0, "one live-history row");
    assert_eq!(
        metrics.total_px, 80.0,
        "frozen 30 + live (history 10 + 2 rows + 2 pads)"
    );
    assert_eq!(metrics.offset_px, 33.0);
    assert_eq!(metrics.last_item_px, 30.0);
}

#[test]
fn selected_item_tracks_store_head_eviction() {
    assert_eq!(
        block_list::shift_selected_item_for_eviction(Some(4), 2, 10),
        Some(2)
    );
    assert_eq!(
        block_list::shift_selected_item_for_eviction(Some(1), 2, 10),
        None
    );
    assert_eq!(
        block_list::shift_selected_item_for_eviction(Some(10), 3, 7),
        Some(7),
        "old live index shifts to the new live index"
    );
    assert_eq!(
        block_list::shift_selected_item_for_eviction(Some(11), 3, 7),
        None
    );
}

#[test]
fn list_reconcile_covers_eviction_growth_and_resets() {
    use crate::block_list::{ListReconcile, plan_list_reconcile};

    // Unchanged mirror: nothing to do.
    assert_eq!(
        plan_list_reconcile(5, 0, 5),
        ListReconcile::Patch {
            front_evict: 0,
            tail_splice: None
        }
    );
    // Growth only: replace the old live tail with the new blocks + tail.
    assert_eq!(
        plan_list_reconcile(5, 0, 7),
        ListReconcile::Patch {
            front_evict: 0,
            tail_splice: Some((4..5, 3))
        }
    );
    // First render from an empty mirror.
    assert_eq!(
        plan_list_reconcile(0, 0, 3),
        ListReconcile::Patch {
            front_evict: 0,
            tail_splice: Some((0..0, 3))
        }
    );
    // Pure eviction: mirror shrinks from the front, counts line up.
    assert_eq!(
        plan_list_reconcile(5, 2, 3),
        ListReconcile::Patch {
            front_evict: 2,
            tail_splice: None
        }
    );
    // Eviction plus growth in the same frame.
    assert_eq!(
        plan_list_reconcile(5, 2, 6),
        ListReconcile::Patch {
            front_evict: 2,
            tail_splice: Some((2..3, 4))
        }
    );
    // Eviction beyond the mirrored frozen items: mirror too stale.
    assert_eq!(plan_list_reconcile(3, 5, 4), ListReconcile::Reset);
    // Shrink that eviction does not explain (history cleared).
    assert_eq!(plan_list_reconcile(5, 0, 2), ListReconcile::Reset);
    assert_eq!(plan_list_reconcile(5, 1, 2), ListReconcile::Reset);
}

#[test]
fn remeasure_scope_tracks_layout_vs_content_changes() {
    use crate::block_list::reconcile::plan_remeasure;
    use crate::block_list::{BlockListMeasureKey, RemeasureScope};

    let key = BlockListMeasureKey {
        layout: (80, 16.0, 1.0),
        store_len: 3,
        evicted_items: 0,
        last_item_px: 32.0,
        tail_px: 0.0,
        live_rows: 24,
    };

    assert_eq!(plan_remeasure(None, key), RemeasureScope::Tail);
    assert_eq!(plan_remeasure(Some(key), key), RemeasureScope::None);

    let grown = BlockListMeasureKey {
        store_len: 4,
        ..key
    };
    assert_eq!(plan_remeasure(Some(key), grown), RemeasureScope::Tail);

    let relaid = BlockListMeasureKey {
        layout: (100, 16.0, 1.0),
        ..key
    };
    assert_eq!(plan_remeasure(Some(key), relaid), RemeasureScope::All);
}

#[test]
fn block_list_alignment_follows_input_style_anchor() {
    assert_eq!(block_list::block_list_alignment(false), ListAlignment::Top);
    assert_eq!(
        block_list::block_list_alignment(true),
        ListAlignment::Bottom
    );
}

#[test]
fn block_list_live_chrome_marks_idle_open_prompt() {
    let chrome = block_list::block_list_live_chrome(4, 2, 10.0, None, true, false).unwrap();
    assert_eq!(chrome.item, 4);
    assert_eq!(chrome.accent, theme::BLOCK_INPUT_COLOR);
    assert_eq!(chrome.header, None);
    assert!(!chrome.selected);

    assert!(block_list::block_list_live_chrome(4, 2, 10.0, None, false, false).is_none());
}

#[test]
fn frozen_chrome_offset_moves_header_with_item() {
    let chrome = block_list::FrozenItemChrome {
        item: 0,
        top: 0.0,
        bottom: 40.0,
        header_y: 10.0,
        accent: theme::BLOCK_SUCCESS_COLOR,
        header: Some("build · ✓".into()),
        selected: false,
    };

    let chrome = block_list::offset_frozen_chrome(chrome, 80.0);
    assert_eq!(
        (chrome.top, chrome.bottom, chrome.header_y),
        (80.0, 120.0, 90.0)
    );
}
