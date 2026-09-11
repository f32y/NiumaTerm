use gpui::Context;

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

    pub(crate) fn apply_scroll_outcome(
        &mut self,
        outcome: ScrollOutcome,
        cx: &mut Context<Self>,
    ) -> bool {
        match outcome {
            ScrollOutcome::Ignored => return false,
            ScrollOutcome::GridRequested => self.invalidate(cx),
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
}
