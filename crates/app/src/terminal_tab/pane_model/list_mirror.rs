use std::ops::Range;

use nmt_terminal::block_store::BlockStore;

use crate::terminal_tab::block_list::reconcile::{BlockListRenderMetrics, plan_remeasure};
use crate::terminal_tab::block_list::{
    BlockListMeasureKey, ListReconcile, RemeasureScope, item_px, live_item_px, plan_list_reconcile,
};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct ListPosition {
    pub item_ix: usize,
    pub offset_px: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ListOp {
    Reset(usize),
    Splice(Range<usize>, usize),
    RemeasureAll,
    Remeasure(Range<usize>),
    ScrollTo(ListPosition),
    ScrollToEnd,
}

pub(crate) struct BlockListMirror {
    pub item_count: usize,
    pub evicted_items: u64,
    pub scrollbar: (f32, f32),
    pub active_top: f32,
    last_measure_key: Option<BlockListMeasureKey>,
}

impl Default for BlockListMirror {
    fn default() -> Self {
        Self {
            item_count: 1,
            evicted_items: 0,
            scrollbar: (0.0, 0.0),
            active_top: 0.0,
            last_measure_key: None,
        }
    }
}

impl BlockListMirror {
    pub(crate) fn sync(
        &mut self,
        metrics: &BlockListRenderMetrics,
        layout: (u32, f32, f32),
        live_rows: usize,
    ) -> Vec<ListOp> {
        let mut ops = Vec::new();

        let evicted = metrics.evicted_items.saturating_sub(self.evicted_items) as usize;

        match plan_list_reconcile(self.item_count, evicted, metrics.item_count) {
            ListReconcile::Reset => ops.push(ListOp::Reset(metrics.item_count)),
            ListReconcile::Patch {
                front_evict,
                tail_splice,
            } => {
                if front_evict > 0 {
                    ops.push(ListOp::Splice(0..front_evict, 0));
                }

                if let Some((range, count)) = tail_splice {
                    ops.push(ListOp::Splice(range, count));
                }
            }
        }

        let key = BlockListMeasureKey {
            layout,
            store_len: metrics.store_len,
            evicted_items: metrics.evicted_items,
            last_item_px: metrics.last_item_px,
            tail_px: metrics.tail_px,
            live_rows,
        };

        match plan_remeasure(self.last_measure_key, key) {
            RemeasureScope::All => ops.push(ListOp::RemeasureAll),
            RemeasureScope::Tail => ops.push(ListOp::Remeasure(
                metrics.store_len.saturating_sub(1)..metrics.item_count,
            )),
            RemeasureScope::None => {}
        }

        self.last_measure_key = Some(key);
        self.item_count = metrics.item_count;
        self.evicted_items = metrics.evicted_items;

        ops
    }

    pub(crate) fn scroll_to_px(
        store: &BlockStore,
        history_rows: u64,
        live_rows: usize,
        layout: (u32, f32, f32),
        target: f32,
    ) -> ListOp {
        let (cols, cell_h, pad_rows) = layout;

        let mut y = 0.0;

        for (ix, item) in store.items().iter().enumerate() {
            let h = item_px(item, cols, cell_h, pad_rows);

            if target < y + h {
                return ListOp::ScrollTo(ListPosition {
                    item_ix: ix,
                    offset_px: (target - y).max(0.0),
                });
            }

            y += h;
        }

        if target < y + live_item_px(history_rows, live_rows, cell_h, pad_rows) {
            ListOp::ScrollTo(ListPosition {
                item_ix: store.items().len(),
                offset_px: (target - y).max(0.0),
            })
        } else {
            ListOp::ScrollToEnd
        }
    }
}
