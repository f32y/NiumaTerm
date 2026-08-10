use crate::terminal::block_list::*;

pub(crate) struct BlockListState {
    /// Native GPUI list state for block-split rendering.
    pub list: ListState,
    /// Last item count mirrored into `list`.
    pub item_count: usize,
    /// Last store eviction counter mirrored into `list`.
    pub evicted_items: u64,
    /// The native list scroll callback is stable for the pane; install once.
    pub scroll_handler_set: bool,
    /// Pixel mirror of native list scroll: `(scroll_top, max_scroll)`.
    pub scrollbar: (f32, f32),
    /// Element-local top of the live grid, even outside list prepaint overdraw.
    pub active_top: f32,
}

impl BlockListState {
    pub(crate) fn new(alignment: ListAlignment) -> Self {
        let list = ListState::new(1, alignment, px(240.0));
        list.set_follow_mode(FollowMode::Tail);
        Self {
            list,
            item_count: 1,
            evicted_items: 0,
            scroll_handler_set: false,
            scrollbar: (0.0, 0.0),
            active_top: 0.0,
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) struct BlockListMeasureKey {
    /// (cols, cell height, pad rows) — pad rows toggling (Command Blocks
    /// on/off) changes every item height, so it must force a full remeasure.
    pub(crate) layout: (u32, f32, f32),
    pub(crate) store_len: usize,
    pub(crate) evicted_items: u64,
    pub(crate) last_item_px: f32,
    pub(crate) tail_px: f32,
    pub(crate) live_rows: usize,
}

pub(crate) struct BlockListRenderMetrics {
    pub(crate) store_len: usize,
    pub(crate) evicted_items: u64,
    pub(crate) item_count: usize,
    pub(crate) frozen_px: f32,
    /// The live item's history rows in pixels (active-grid scrollback above
    /// the live grid) — the "tail" position in scroll/active-top math.
    pub(crate) tail_px: f32,
    pub(crate) total_px: f32,
    pub(crate) offset_px: f32,
    pub(crate) last_item_px: f32,
}

pub(crate) fn block_list_render_metrics(
    store: &BlockStore,
    live_rows: usize,
    history_rows: u64,
    cols: u32,
    cell_h: f32,
    pad_rows: f32,
    offset: ListOffset,
) -> BlockListRenderMetrics {
    let items = store.items();
    let store_len = items.len();
    let item_count = store_len + 1;
    let mut frozen_px = 0.0;
    let mut offset_px = 0.0;
    let mut last_item_px = 0.0;

    for (ix, item) in items.iter().enumerate() {
        let item_px = terminal::block_list::item_px(item, cols, cell_h, pad_rows);
        if ix < offset.item_ix {
            offset_px += item_px;
        }
        if ix + 1 == store_len {
            last_item_px = item_px;
        }
        frozen_px += item_px;
    }

    let tail_px = history_rows as f32 * cell_h;
    let total_px =
        frozen_px + terminal::block_list::live_item_px(history_rows, live_rows, cell_h, pad_rows);
    if offset.item_ix >= item_count {
        offset_px = total_px;
    } else if offset.item_ix <= store_len {
        offset_px += offset.offset_in_item.as_f32();
    }

    BlockListRenderMetrics {
        store_len,
        evicted_items: store.evicted_items,
        item_count,
        frozen_px,
        tail_px,
        total_px,
        offset_px,
        last_item_px,
    }
}

pub(crate) fn shift_selected_item_for_eviction(
    selected: Option<usize>,
    evicted_delta: usize,
    store_len: usize,
) -> Option<usize> {
    let selected = selected?;
    if selected < evicted_delta {
        None
    } else {
        let shifted = selected - evicted_delta;
        (shifted <= store_len).then_some(shifted)
    }
}

/// How to bring the mirrored GPUI `ListState` in line with the store after
/// front evictions and tail growth. Pure so the index arithmetic is testable
/// away from `ListState`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ListReconcile {
    /// Replace the mirror wholesale with the new item count.
    Reset,
    /// Drop `front_evict` items from the front, then replace the
    /// `tail_splice` range with that many new items.
    Patch {
        front_evict: usize,
        tail_splice: Option<(ops::Range<usize>, usize)>,
    },
}

pub(crate) fn plan_list_reconcile(
    mirrored_count: usize,
    evicted_delta: usize,
    item_count: usize,
) -> ListReconcile {
    let mut mirrored = mirrored_count;
    let mut front_evict = 0;

    if evicted_delta > 0 {
        // Only frozen items (all but the live tail) can be evicted from the
        // mirror; a delta beyond that means the mirror is too stale to patch.
        let old_frozen = mirrored.saturating_sub(1);

        if evicted_delta > old_frozen {
            return ListReconcile::Reset;
        }

        front_evict = evicted_delta;
        mirrored -= evicted_delta;
    }

    // A shrink beyond eviction (e.g. history cleared) invalidates the mirror.
    if item_count < mirrored {
        return ListReconcile::Reset;
    }

    let tail_splice = (item_count != mirrored).then(|| {
        // Replace the old live tail; the new items are the freshly frozen
        // blocks plus the new live tail.
        let old_live = mirrored.saturating_sub(1);
        (old_live..mirrored, item_count - old_live)
    });

    ListReconcile::Patch {
        front_evict,
        tail_splice,
    }
}

/// What the mirrored list must remeasure after this frame's metrics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RemeasureScope {
    /// Layout inputs (cols/cell/pad) changed: every item height is stale.
    All,
    /// Content changed: only the last frozen item and the live tail moved.
    Tail,
    None,
}

pub(crate) fn plan_remeasure(
    prev: Option<BlockListMeasureKey>,
    next: BlockListMeasureKey,
) -> RemeasureScope {
    if prev.is_some_and(|prev| prev.layout != next.layout) {
        RemeasureScope::All
    } else if prev != Some(next) {
        RemeasureScope::Tail
    } else {
        RemeasureScope::None
    }
}
