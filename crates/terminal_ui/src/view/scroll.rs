use gpui::{Context, Pixels};

use crate::pane_model::scroll::ScrollOutcome;
use crate::scrollbar::geometry::SCROLLBAR_AUTO_HIDE_DELAY;
use crate::view::TerminalPane;

impl TerminalPane {
    pub(crate) fn mark_scrollbar_activity(&mut self, cx: &mut Context<Self>) {
        let generation = self.model.scrollbar.mark_activity();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(SCROLLBAR_AUTO_HIDE_DELAY)
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.model.scrollbar.should_fade(generation) {
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn scrollbar_fraction(&self, y: Pixels) -> f32 {
        let bounds = self.content_bounds.unwrap_or_default();
        ((y.as_f32() - bounds.origin.y.as_f32()) / bounds.size.height.as_f32().max(1.0))
            .clamp(0.0, 1.0)
    }

    pub(super) fn apply_scroll_outcome(
        &mut self,
        outcome: ScrollOutcome,
        cx: &mut Context<Self>,
    ) -> bool {
        match outcome {
            ScrollOutcome::Ignored => return false,
            ScrollOutcome::GridChanged => self.invalidate(cx),
            ScrollOutcome::List(op) => {
                self.block_list.apply(op);
                cx.notify();
            }
        }
        self.mark_scrollbar_activity(cx);
        true
    }

    pub(super) fn scroll_to_latest(&mut self, cx: &mut Context<Self>) -> bool {
        let outcome = self.model.scroll_to_latest();
        self.apply_scroll_outcome(outcome, cx)
    }

    pub(crate) fn scroll_thumb_to(&mut self, thumb_top: f32, cx: &mut Context<Self>) {
        let outcome = self.model.scroll_thumb_to(thumb_top);
        self.apply_scroll_outcome(outcome, cx);
    }
}
