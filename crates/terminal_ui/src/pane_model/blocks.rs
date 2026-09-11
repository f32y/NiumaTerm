use crate::block_list::{BlockListPoint, block_list_active_top_px, block_list_render_metrics};
use crate::frame::TerminalFrame;
use crate::layout::{frame_content_rows, live_frame_text};
use crate::metrics::CellMetrics;
use crate::pane_model::PaneController;
use crate::pane_model::list_mirror::{ListOp, ListPosition};
use crate::pane_model::viewport::LocalPoint;

pub(crate) struct ListPlan {
    pub ops: Vec<ListOp>,
    pub history_rows: u64,
    pub live_index: usize,
    pub cols: u32,
}

impl PaneController {
    pub(crate) fn live_history_rows(&self, frame: &TerminalFrame) -> u64 {
        if !self.source.session.engine_blocks() {
            return 0;
        }
        let sb = frame.scrollbar();
        sb.total.saturating_sub(sb.len)
    }

    pub(crate) fn block_list_point_at(&self, local: LocalPoint) -> Option<BlockListPoint> {
        if !self.block_list_mode() || local.y >= self.frozen.active_top() {
            return None;
        }
        let cell = self.cell_metrics?;
        self.frozen.hit_test(
            local.x,
            local.y,
            cell.width_px,
            cell.height_px,
            self.content_cols(),
            self.settings.pad_rows,
        )
    }

    pub(crate) fn selected_block_command(&self) -> Option<String> {
        self.source.session.block_command(self.gutter.selected()?)
    }

    pub(crate) fn selected_block_output(&self) -> Option<String> {
        let item = self.gutter.selected()?;
        let live = item == self.source.session.block_store().lock().items().len();
        if live {
            self.frame_cache
                .current()
                .and_then(|frame| live_frame_text(&frame))
        } else {
            self.source.session.block_text(item)
        }
    }

    pub(crate) fn prepare_block_list(
        &mut self,
        frame: &TerminalFrame,
        cell: CellMetrics,
        viewport_px: f32,
        position: ListPosition,
    ) -> Option<ListPlan> {
        if !self.block_list_mode() {
            return None;
        }
        let cols = self.content_cols();
        let live_rows = frame_content_rows(frame);
        let history_rows = self.live_history_rows(frame);
        let store = self.source.session.block_store();
        let metrics = block_list_render_metrics(
            &store.lock(),
            live_rows,
            history_rows,
            cols,
            cell.height_px,
            self.settings.pad_rows,
            position,
        );
        let evicted = metrics
            .evicted_items
            .saturating_sub(self.block_list.evicted_items) as usize;
        self.gutter.shift_for_eviction(evicted, metrics.store_len);
        let ops = self.block_list.sync(
            &metrics,
            (cols, cell.height_px, self.settings.pad_rows),
            live_rows,
        );
        let max_scroll = (metrics.total_px - viewport_px).max(0.0);
        let offset = metrics.offset_px.min(max_scroll);
        self.block_list.scrollbar = (offset, max_scroll);
        self.block_list.active_top = block_list_active_top_px(
            metrics.frozen_px,
            metrics.tail_px,
            cell.height_px,
            self.settings.pad_rows,
            offset,
        );
        self.frozen.set_active_top(self.block_list.active_top);
        self.update_viewport();
        Some(ListPlan {
            ops,
            history_rows,
            live_index: metrics.store_len,
            cols,
        })
    }
}
