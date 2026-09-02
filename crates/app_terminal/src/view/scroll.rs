use gpui_component::scroll::SCROLLBAR_AUTO_HIDE_DELAY;

use crate::scrollbar::scrollbar_opacity;
use crate::view::*;

pub(crate) fn scroll_block_list_to_latest(block_list: &mut BlockListState) -> bool {
    let (offset, max) = block_list.scrollbar;

    if offset >= max {
        return false;
    }

    block_list.list.scroll_to_end();
    block_list.scrollbar.0 = max;

    true
}

pub(crate) fn viewport_is_scrolled(offset: u64, total: u64, len: u64) -> bool {
    offset < total.saturating_sub(len)
}

/// Scrollbar drag and fade state. The bar is opaque while the thumb is held
/// and for a linger window after the last scroll, then fades out, so the drag
/// flag and the activity clock have to be read together to paint a frame.
#[derive(Default)]
pub(crate) struct ScrollbarActivity {
    /// True while the scrollbar thumb is being dragged (mouse-move then scrolls
    /// to the pointer instead of selecting text).
    dragging: bool,
    /// Pointer offset inside the thumb at drag start (track fraction), so
    /// grabbing the thumb does not jump it.
    grab: f32,
    /// Last user scroll action; the scrollbar stays opaque within
    /// [`SCROLLBAR_AUTO_HIDE_DELAY`], then fades out unless it is being dragged.
    last_activity: Option<time::Instant>,
    /// Bumped per scroll action so only the newest hide-timer repaints.
    activity_gen: u64,
}

impl ScrollbarActivity {
    /// Record a user scroll action and schedule the repaint that starts fading
    /// the scrollbar once [`SCROLLBAR_AUTO_HIDE_DELAY`] passes without further
    /// activity.
    pub(crate) fn mark_activity(&mut self, cx: &mut Context<TerminalPane>) {
        self.last_activity = Some(time::Instant::now());
        self.activity_gen += 1;

        let generation = self.activity_gen;

        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(SCROLLBAR_AUTO_HIDE_DELAY)
                .await;

            let _ = this.update(cx, |this, cx| {
                // Stale timers from earlier scroll ticks no-op; only the newest
                // one repaints (with the linger expired, starting fade-out).
                if this.scrollbar.activity_gen == generation && !this.scrollbar.dragging {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Start a thumb drag, remembering where inside the thumb the pointer
    /// landed as a track fraction.
    pub(crate) fn begin_drag(&mut self, grab: f32) {
        self.dragging = true;
        self.grab = grab;
    }

    pub(crate) fn end_drag(&mut self) {
        self.dragging = false;
    }

    pub(crate) fn is_dragging(&self) -> bool {
        self.dragging
    }

    /// Where the thumb top belongs for a pointer at `fraction` of the track,
    /// so the grabbed point stays under the pointer for the whole drag.
    pub(crate) fn thumb_top_for(&self, fraction: f32) -> f32 {
        fraction - self.grab
    }

    /// Opacity for this frame, or `None` once the bar has faded out completely
    /// and should not be painted at all.
    pub(super) fn opacity(&self) -> Option<f32> {
        scrollbar_opacity(self.dragging, self.last_activity.map(|at| at.elapsed()))
    }
}

impl TerminalPane {
    /// Map a window-y pointer position to a 0..1 fraction of the content height.
    pub(crate) fn scrollbar_fraction(&self, y: Pixels) -> f32 {
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

            self.scrollbar.mark_activity(cx);

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
    pub(crate) fn scroll_thumb_to(&mut self, thumb_top: f32, cx: &mut Context<Self>) {
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

            self.scrollbar.mark_activity(cx);

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
            self.scrollbar.mark_activity(cx);

            self.invalidate(cx);
        }
    }
}
