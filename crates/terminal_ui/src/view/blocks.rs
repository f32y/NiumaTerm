use gpui::prelude::*;
use gpui::{AnyElement, Bounds, Context, Pixels, Window, list};

use crate::frame::TerminalFrame;
use crate::metrics::CellMetrics;
use crate::pane_model::list_mirror::ListPosition;
use crate::terminal_view::BlockListItem;
use crate::view::{
    CopyBlockCommand, CopyBlockOutput, NextBlock, PreviousBlock, RerunBlock, TerminalPane,
};

impl TerminalPane {
    pub(crate) fn begin_block_list_frame(
        &mut self,
        bounds: Bounds<Pixels>,
        cell: CellMetrics,
        cx: &mut Context<Self>,
    ) {
        self.set_content_bounds(bounds, cell, cx);
        self.model
            .frozen
            .begin_frame(self.model.block_list.active_top);
    }

    pub(super) fn on_copy_block_command(
        &mut self,
        _: &CopyBlockCommand,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(command) = self.model.selected_block_command()
            && self.model.copy_text_to_clipboard(command)
        {
            cx.notify();
        }
    }

    pub(super) fn on_copy_block_output(
        &mut self,
        _: &CopyBlockOutput,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(text) = self.model.selected_block_output()
            && self.model.copy_text_to_clipboard(text)
        {
            cx.notify();
        }
    }

    pub(super) fn on_rerun_block(
        &mut self,
        _: &RerunBlock,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(command) = self.model.selected_block_command()
            && self
                .model
                .source
                .session
                .write_text(&format!("{command}\r"))
        {
            self.invalidate(cx);
        }
    }

    pub(super) fn on_previous_block(
        &mut self,
        _: &PreviousBlock,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let outcome = self.model.jump_to_block(-1);
        self.apply_scroll_outcome(outcome, cx);
    }

    pub(super) fn on_next_block(&mut self, _: &NextBlock, _: &mut Window, cx: &mut Context<Self>) {
        let outcome = self.model.jump_to_block(1);
        self.apply_scroll_outcome(outcome, cx);
    }

    pub(super) fn render_block_list_content(
        &mut self,
        frame: &TerminalFrame,
        cell: CellMetrics,
        viewport_px: f32,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let offset = self.block_list.list.logical_scroll_top();
        let plan = self.model.prepare_block_list(
            frame,
            cell,
            viewport_px,
            ListPosition {
                item_ix: offset.item_ix,
                offset_px: offset.offset_in_item.as_f32(),
            },
        )?;
        for op in plan.ops {
            self.block_list.apply(op);
        }
        if !self.block_list.scroll_handler_set {
            let pane = cx.entity();
            self.block_list.list.set_scroll_handler(move |_, _, cx| {
                pane.update(cx, |pane, cx| pane.mark_scrollbar_activity(cx));
            });
            self.block_list.scroll_handler_set = true;
        }
        Some(self.block_list_element(
            frame,
            cell,
            plan.cols,
            plan.history_rows,
            plan.live_index,
            cx,
        ))
    }

    fn block_list_element(
        &self,
        frame: &TerminalFrame,
        cell: CellMetrics,
        cols: u32,
        history_rows: u64,
        live_index: usize,
        cx: &Context<Self>,
    ) -> AnyElement {
        let frame_for_items = frame.clone();
        let in_flight_for_items = self.model.in_flight.clone();
        let has_open_prompt_for_items = self.model.open_prompt;
        let selected_frozen_item = self.model.gutter.selected();
        let frozen_selection = self.model.frozen_drag.current();
        let cell_for_items = cell;
        let pane_for_items = cx.entity();
        let store_for_items = self.model.source.session.block_store();

        list(self.block_list.list.clone(), move |ix, _window, _cx| {
            if ix < live_index {
                BlockListItem::Frozen {
                    item_idx: ix,
                    store: store_for_items.clone(),
                    cols,
                    cell: cell_for_items,
                    selection: frozen_selection,
                    selected_item: selected_frozen_item,
                    pane: pane_for_items.clone(),
                }
                .into_any_element()
            } else {
                BlockListItem::Live {
                    frame: frame_for_items.clone(),
                    history_rows,
                    in_flight: in_flight_for_items.clone(),
                    has_open_prompt: has_open_prompt_for_items,
                    live_index,
                    selected_item: selected_frozen_item,
                    cols,
                    cell: cell_for_items,
                    pane: pane_for_items.clone(),
                }
                .into_any_element()
            }
        })
        .size_full()
        .into_any_element()
    }
}
