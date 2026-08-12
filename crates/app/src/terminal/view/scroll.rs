use crate::terminal::view::*;

pub(in crate::terminal) fn scroll_block_list_to_latest(block_list: &mut BlockListState) -> bool {
    let (offset, max) = block_list.scrollbar;

    if offset >= max {
        return false;
    }

    block_list.list.scroll_to_end();
    block_list.scrollbar.0 = max;

    true
}

pub(in crate::terminal) fn viewport_is_scrolled(offset: u64, total: u64, len: u64) -> bool {
    offset < total.saturating_sub(len)
}

impl TerminalPane {
    /// Record a user scroll action and schedule the repaint that starts fading
    /// the scrollbar once [`SCROLLBAR_LINGER`] passes without further activity.
    pub(in crate::terminal) fn mark_scroll_activity(&mut self, cx: &mut Context<Self>) {
        self.last_scroll_activity = Some(time::Instant::now());
        self.scroll_activity_gen += 1;

        let generation = self.scroll_activity_gen;

        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SCROLLBAR_LINGER).await;

            let _ = this.update(cx, |this, cx| {
                // Stale timers from earlier scroll ticks no-op; only the newest
                // one repaints (with the linger expired, starting fade-out).
                if this.scroll_activity_gen == generation && !this.scrollbar_dragging {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Map a window-y pointer position to a 0..1 fraction of the content height.
    pub(in crate::terminal) fn scrollbar_fraction(&self, y: Pixels) -> f32 {
        let bounds = self.content_bounds.unwrap_or_default();
        let height = bounds.size.height.as_f32().max(1.0);

        ((y.as_f32() - bounds.origin.y.as_f32()) / height).clamp(0.0, 1.0)
    }

    /// Restore the newest output only when the viewport has actually moved,
    /// leaving End available for normal shell line navigation at the bottom.
    pub(super) fn scroll_to_latest(&mut self, cx: &mut Context<Self>) -> bool {
        if self.block_list_mode(cx) {
            if !scroll_block_list_to_latest(&mut self.block_list) {
                return false;
            }

            self.mark_scroll_activity(cx);

            cx.notify();

            return true;
        }

        let scrolled = self.frame_cache.current().is_some_and(|frame| {
            let scrollbar = frame.scrollbar();

            viewport_is_scrolled(scrollbar.offset, scrollbar.total, scrollbar.len)
        });

        if scrolled {
            self.scroll_thumb_to(1.0, cx);
        }

        scrolled
    }

    /// Scroll so the thumb's top sits at `thumb_top` of the track.
    pub(in crate::terminal) fn scroll_thumb_to(&mut self, thumb_top: f32, cx: &mut Context<Self>) {
        if self.block_list_mode(cx) {
            let (_, max_scroll) = self.block_list.scrollbar;
            let viewport = self
                .content_bounds
                .map(|b| b.size.height.as_f32())
                .unwrap_or(0.0);
            let total = max_scroll + viewport;

            let Some(new) = scrollbar_offset_for_thumb(total as f64, viewport as f64, thumb_top)
            else {
                return;
            };

            let new = new as f32;

            if let (Some(frame), Some(cell)) = (self.frame_cache.current(), self.cell_metrics) {
                let cols = self.content_cols();
                let pad_rows = block_pad_rows(cx);
                let store = self.surface.block_store();
                let store = store.lock();
                let offset =
                    self.list_offset_for_px(&store, &frame, cols, cell.height_px, pad_rows, new);
                self.block_list.list.scroll_to(offset);
                self.block_list.scrollbar.0 = new;
            }

            self.mark_scroll_activity(cx);

            cx.notify();

            return;
        }

        let sb = self
            .frame_cache
            .current()
            .map(|frame| frame.scrollbar())
            .unwrap_or_default();

        let scrollable = sb.total.saturating_sub(sb.len);

        if scrollable == 0 {
            return;
        }

        let target = scrollbar_offset_for_thumb(sb.total as f64, sb.len as f64, thumb_top)
            .unwrap_or_default()
            .round() as u64;

        let delta = target as isize - sb.offset as isize;

        if delta != 0 && self.surface.scroll_lines(delta) {
            self.mark_scroll_activity(cx);

            self.invalidate(cx);
        }
    }
}
