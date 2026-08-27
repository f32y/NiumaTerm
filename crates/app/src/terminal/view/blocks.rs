use crate::terminal::view::mouse::block_gutter_hit;
use crate::terminal::view::*;

impl TerminalPane {
    /// Engine blocks remain the storage model while the setting controls only
    /// their presentation, so display changes never hide frozen output.
    /// Alt-screen stays a plain terminal grid.
    pub(in crate::terminal) fn block_list_mode(&self, _cx: &App) -> bool {
        self.surface.engine_blocks() && !self.surface.alt_screen()
    }

    pub(super) fn block_chrome_enabled(&self, cx: &App) -> bool {
        self.block_list_mode(cx) && cx.global::<AppSettings>().command_blocks
    }

    /// Columns of the content area (block-split hit-testing).
    pub(super) fn content_cols(&self) -> u32 {
        match (self.content_bounds, self.cell_metrics) {
            (Some(b), Some(cell)) => {
                (b.size.width.as_f32() / cell.width_px).floor().max(1.0) as u32
            }
            _ => 80,
        }
    }

    fn block_list_total_px(
        &self,
        store: &BlockStore,
        frame: &TerminalFrame,
        cols: u32,
        cell_h: f32,
        pad_rows: f32,
    ) -> f32 {
        let frozen: f32 = store
            .items()
            .iter()
            .map(|item| terminal::block_list::item_px(item, cols, cell_h, pad_rows))
            .sum();

        frozen
            + self.live_history_rows(frame) as f32 * cell_h
            + frame_content_rows(frame) as f32 * cell_h
    }

    /// The active grid's scrollback rows, rendered above the live grid
    /// inside the live item when scrolling into a running command.
    /// 0 in classic single-grid mode (no block list is shown there anyway).
    fn live_history_rows(&self, frame: &TerminalFrame) -> u64 {
        if !self.surface.engine_blocks() {
            return 0;
        }

        let sb = frame.scrollbar();

        sb.total.saturating_sub(sb.len)
    }

    pub(super) fn list_offset_for_px(
        &self,
        store: &BlockStore,
        frame: &TerminalFrame,
        cols: u32,
        cell_h: f32,
        pad_rows: f32,
        target: f32,
    ) -> ListOffset {
        let mut y = 0.0f32;

        for (ix, item) in store.items().iter().enumerate() {
            let h = terminal::block_list::item_px(item, cols, cell_h, pad_rows);
            if target < y + h {
                return ListOffset {
                    item_ix: ix,
                    offset_in_item: px((target - y).max(0.0)),
                };
            }
            y += h;
        }

        let live_h = self.block_list_total_px(store, frame, cols, cell_h, pad_rows) - y;

        if target < y + live_h {
            ListOffset {
                item_ix: store.items().len(),
                offset_in_item: px((target - y).max(0.0)),
            }
        } else {
            ListOffset {
                item_ix: store.items().len() + 1,
                offset_in_item: px(0.0),
            }
        }
    }

    pub(in crate::terminal) fn begin_block_list_frame(
        &mut self,
        bounds: Bounds<Pixels>,
        cell: metrics::CellMetrics,
        cx: &mut Context<Self>,
    ) {
        self.set_content_bounds(bounds, cell, cx);

        self.frozen_hit.clear();
        self.frozen_hit.set_active_top(self.block_list.active_top);
        self.frozen_chrome.clear();
        self.frozen_separators.clear();
    }

    pub(in crate::terminal) fn record_frozen_view(
        &mut self,
        view: &terminal::block_list::FrozenView,
        item_top: f32,
    ) {
        self.frozen_separators
            .extend(view.separators.iter().map(|y| item_top + y));

        for row in &view.rows {
            self.frozen_hit
                .push_row(item_top + row.y, row.item, row.row, row.cell_count);
        }

        for chrome in &view.items_chrome {
            self.frozen_chrome
                .push(offset_frozen_chrome(chrome.clone(), item_top));
        }
    }

    pub(in crate::terminal) fn record_frozen_chrome(
        &mut self,
        chrome: terminal::block_list::FrozenItemChrome,
        item_top: f32,
    ) {
        self.frozen_chrome
            .push(offset_frozen_chrome(chrome, item_top));
    }

    /// Map a window position to either an immutable block row or an absolute
    /// SCREEN row from the active block's history.
    pub(in crate::terminal) fn block_list_point_at(
        &self,
        position: Point<Pixels>,
        cx: &App,
    ) -> Option<BlockListPoint> {
        if !self.block_list_mode(cx) {
            return None;
        }

        let cell = self.cell_metrics?;
        let origin = self.content_origin();
        let local_x = (position.x - origin.x).as_f32();
        let local_y = (position.y - origin.y).as_f32();

        if local_y >= self.frozen_hit.active_top {
            return None;
        }

        self.frozen_hit.hit_test(
            local_x,
            local_y,
            cell.width_px,
            cell.height_px,
            self.content_cols(),
            block_pad_rows(cx),
        )
    }

    /// Handle a mouse-down as a gutter selection in the block list. Frozen
    /// and live items both report chrome from list item bounds. Returns `true`
    /// when the click selected something; a click elsewhere clears selection
    /// and falls through.
    pub(super) fn try_select_frozen_item(
        &mut self,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.surface.mouse_reporting_active() {
            return false;
        }

        let origin = self.content_origin();

        if block_gutter_hit(position.x.as_f32(), origin.x.as_f32()) {
            let y = (position.y - origin.y).as_f32();

            let hit = self
                .frozen_chrome
                .iter()
                .find(|chrome| (chrome.top..chrome.bottom).contains(&y))
                .map(|chrome| chrome.item);

            if let Some(item) = hit {
                self.selected_frozen_item = Some(item);

                cx.notify();

                return true;
            }
        }

        let cleared_frozen = self.selected_frozen_item.take().is_some();

        if cleared_frozen {
            cx.notify();
        }

        false
    }

    /// Scroll the list so the previous/next non-filler item's top lands at
    /// the viewport top; no-op at the edges (list-mode Previous/NextBlock).
    fn jump_to_frozen_item(&mut self, direction: i8, cx: &mut Context<Self>) {
        let Some(cell) = self.cell_metrics else {
            return;
        };

        let cols = self.content_cols();
        let pad_rows = block_pad_rows(cx);
        let (resolved, max_scroll) = self.block_list.scrollbar;
        let store = self.surface.block_store();

        // One lock hold for both reads: target resolution and offset
        // conversion see the same block-list state.
        let store = store.lock();

        let Some(target) = terminal::block_list::nav_item_top(
            &store,
            cols,
            cell.height_px,
            pad_rows,
            resolved,
            direction,
        ) else {
            return;
        };

        if let Some(frame) = self.frame_cache.current() {
            self.block_list.list.scroll_to(self.list_offset_for_px(
                &store,
                &frame,
                cols,
                cell.height_px,
                pad_rows,
                target,
            ));
        }

        drop(store);

        self.block_list.scrollbar.0 = target.min(max_scroll);

        self.mark_scroll_activity(cx);

        cx.notify();
    }

    /// Metadata of the selected frozen item (list mode): the command line
    /// when one is known.
    fn selected_frozen_command(&self) -> Option<String> {
        let item = self.selected_frozen_item?;
        let store = self.surface.block_store();
        let store = store.lock();

        if item < store.items().len() {
            return store.items().get(item)?.meta.command.clone();
        }

        if item == store.items().len() {
            return self.in_flight.as_ref().map(|block| block.command.clone());
        }

        None
    }

    fn selected_frozen_output(&self) -> Option<String> {
        let item = self.selected_frozen_item?;
        let store = self.surface.block_store();
        let store = store.lock();

        if item < store.items().len() {
            // Block items format through the engine after the store lock
            // drops (the two locks never nest).
            let handle = store.items().get(item)?.handle()?;

            drop(store);

            return self.format_block_range(handle, None, None);
        }

        let is_live = item == store.items().len();

        drop(store);

        if is_live {
            return self
                .frame_cache
                .current()
                .and_then(|frame| live_frame_text(&frame));
        }

        None
    }

    pub(super) fn expanded_frozen_selection(
        &self,
        point: FrozenPoint,
        selection_type: SelectionType,
    ) -> Option<(FrozenPoint, FrozenPoint)> {
        // The PTY path acquires engine before store, so release the store
        // guard before asking the surface to inspect the engine-owned block.
        let handle = {
            let store = self.surface.block_store();
            let store = store.lock();
            store.items().get(point.item)?.handle()?
        };

        let ((start_line, start_col), (end_line, end_col)) =
            self.surface
                .frozen_selection_range(handle, point.line, point.col, selection_type)?;

        Some((
            FrozenPoint {
                item: point.item,
                line: start_line,
                col: start_col,
            },
            FrozenPoint {
                item: point.item,
                line: end_line,
                col: end_col,
            },
        ))
    }

    /// Resolve a frozen selection to plain text: the per-block ranges are
    /// collected under the store lock, then formatted through acquired
    /// `BlockRef`s after it is released so the store and engine locks are never nested.
    pub(super) fn frozen_selection_to_text(
        &self,
        a: terminal::block_list::FrozenPoint,
        b: terminal::block_list::FrozenPoint,
    ) -> String {
        let pieces = {
            let store = self.surface.block_store();
            let store = store.lock();

            terminal::block_list::frozen_selection_pieces(&store, a, b)
        };

        pieces
            .into_iter()
            .filter_map(|piece| self.format_block_range(piece.handle, piece.start, piece.end))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Export an inclusive cell range of a finished engine block as plain
    /// text (`None` endpoints = the block edge). Soft-wrapped lines rejoin
    /// and trailing blanks trim, matching the legacy line-item copy.
    fn format_block_range(
        &self,
        handle: BlockHandle,
        start: Option<(usize, u32)>,
        end: Option<(usize, u32)>,
    ) -> Option<String> {
        self.surface
            .acquire_block(handle)?
            .block
            .format_range_clamped(start, end, true, true)
    }

    pub(super) fn on_copy_block_command(
        &mut self,
        _: &CopyBlockCommand,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(command) = self.selected_frozen_command() {
            self.surface.copy_text_to_clipboard(command);
            cx.notify();
        }
    }

    pub(super) fn on_copy_block_output(
        &mut self,
        _: &CopyBlockOutput,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = self.selected_frozen_output();

        if let Some(text) = text.filter(|t| !t.is_empty()) {
            self.surface.copy_text_to_clipboard(text);
            cx.notify();
        }
    }

    pub(super) fn on_rerun_block(
        &mut self,
        _: &RerunBlock,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let command = self.selected_frozen_command();

        let Some(command) = command else {
            return;
        };

        self.surface.write_text(&command);
        self.surface.write_text("\r");

        self.invalidate(cx);
    }

    pub(super) fn on_previous_block(
        &mut self,
        _: &PreviousBlock,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.jump_to_frozen_item(-1, cx);
    }

    pub(super) fn on_next_block(&mut self, _: &NextBlock, _: &mut Window, cx: &mut Context<Self>) {
        self.jump_to_frozen_item(1, cx);
    }

    pub(super) fn render_block_list_content(
        &mut self,
        frame: &TerminalFrame,
        cell: metrics::CellMetrics,
        viewport_px: f32,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let block_list_mode = self.block_list_mode(cx);

        if block_list_mode {
            let cols = self.content_cols();
            let pad_rows = block_pad_rows(cx);
            let store = self.surface.block_store();
            let live_rows = frame_content_rows(frame);
            let history_rows = self.live_history_rows(frame);
            let metrics = {
                let store = store.lock();
                block_list_render_metrics(
                    &store,
                    live_rows,
                    history_rows,
                    cols,
                    cell.height_px,
                    pad_rows,
                    self.block_list.list.logical_scroll_top(),
                )
            };

            let store_len = metrics.store_len;
            let evicted_items = metrics.evicted_items;
            let item_count = metrics.item_count;
            let evicted_delta =
                evicted_items.saturating_sub(self.block_list.evicted_items) as usize;

            self.selected_frozen_item = shift_selected_item_for_eviction(
                self.selected_frozen_item,
                evicted_delta,
                store_len,
            );

            match plan_list_reconcile(self.block_list.item_count, evicted_delta, item_count) {
                ListReconcile::Reset => self.block_list.list.reset(item_count),
                ListReconcile::Patch {
                    front_evict,
                    tail_splice,
                } => {
                    if front_evict > 0 {
                        self.block_list.list.splice(0..front_evict, 0);
                    }

                    if let Some((range, count)) = tail_splice {
                        self.block_list.list.splice(range, count);
                    }
                }
            }

            self.block_list.item_count = item_count;
            self.block_list.evicted_items = evicted_items;

            let measure_key = BlockListMeasureKey {
                layout: (cols, cell.height_px, pad_rows),
                store_len,
                evicted_items,
                last_item_px: metrics.last_item_px,
                tail_px: metrics.tail_px,
                live_rows,
            };

            match plan_remeasure(self.last_list_measure_key, measure_key) {
                RemeasureScope::All => self.block_list.list.remeasure(),
                RemeasureScope::Tail => {
                    let start = store_len.saturating_sub(1);
                    self.block_list.list.remeasure_items(start..item_count);
                }
                RemeasureScope::None => {}
            }

            self.last_list_measure_key = Some(measure_key);

            let pane = cx.entity();

            if !self.block_list.scroll_handler_set {
                self.block_list.list.set_scroll_handler({
                    let pane = pane.clone();
                    move |_, _window, cx| {
                        pane.update(cx, |pane, cx| pane.mark_scroll_activity(cx));
                    }
                });

                self.block_list.scroll_handler_set = true;
            }

            let total_px = metrics.total_px;
            let max_scroll = (total_px - viewport_px).max(0.0);
            let offset_px = metrics.offset_px.min(max_scroll);

            self.block_list.scrollbar = (offset_px, max_scroll);
            self.block_list.active_top = block_list_active_top_px(
                metrics.frozen_px,
                metrics.tail_px,
                cell.height_px,
                pad_rows,
                offset_px,
            );

            let frame_for_items = frame.clone();
            let in_flight_for_items = self.in_flight.clone();
            let has_open_prompt_for_items = self.open_prompt;
            let selected_frozen_item = self.selected_frozen_item;
            let frozen_selection = self.frozen_selection;
            let cell_for_items = cell;
            let pane_for_items = pane.clone();
            let store_for_items = store.clone();
            let live_index = item_count.saturating_sub(1);

            Some(
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
                .into_any_element(),
            )
        } else {
            None
        }
    }
}
