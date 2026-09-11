use crate::block_list::nav_item_top;
use crate::layout::frame_content_rows;
use crate::pane_model::PaneController;
use crate::pane_model::list_mirror::{BlockListMirror, ListOp};
use crate::pane_model::viewport::{LocalPoint, Viewport};

#[derive(Debug)]
pub(crate) enum ScrollOutcome {
    Ignored,
    GridRequested,
    List(ListOp),
}

impl PaneController {
    pub(crate) fn scrollbar_mouse_down(
        &mut self,
        position: LocalPoint,
        thumb_top: f32,
        thumb_height: f32,
    ) -> ScrollOutcome {
        let fraction = (position.y / self.content_size.1.max(1.0)).clamp(0.0, 1.0);
        if (thumb_top..thumb_top + thumb_height).contains(&fraction) {
            self.scrollbar.begin_drag(fraction - thumb_top);
            ScrollOutcome::Ignored
        } else {
            self.scrollbar.begin_drag(thumb_height / 2.0);
            self.scroll_thumb_to(self.scrollbar.thumb_top_for(fraction))
        }
    }

    pub(crate) fn scroll_to_latest(&mut self) -> ScrollOutcome {
        if !self.viewport.is_scrolled() {
            return ScrollOutcome::Ignored;
        }
        match self.viewport {
            Viewport::BlockList { .. } => {
                self.block_list.scrollbar.0 = self.block_list.scrollbar.1;
                self.update_viewport();
                ScrollOutcome::List(ListOp::ScrollToEnd)
            }
            Viewport::Grid { .. } => self.scroll_thumb_to(1.0),
        }
    }

    pub(crate) fn scroll_thumb_to(&mut self, thumb_top: f32) -> ScrollOutcome {
        let Some(target) = self.viewport.thumb_target(thumb_top) else {
            return ScrollOutcome::Ignored;
        };
        match &self.viewport {
            Viewport::BlockList { .. } => self.scroll_list_to(target as f32),
            Viewport::Grid { .. } => {
                let accepted = if thumb_top >= 1.0 {
                    self.source.session.scroll_to_end()
                } else {
                    self.source
                        .session
                        .scroll_to(target.round().max(0.0) as u64)
                };
                if accepted {
                    ScrollOutcome::GridRequested
                } else {
                    ScrollOutcome::Ignored
                }
            }
        }
    }

    fn scroll_list_to(&mut self, target: f32) -> ScrollOutcome {
        let (Some(frame), Some(cell)) = (self.frame_cache.current(), self.cell_metrics) else {
            return ScrollOutcome::Ignored;
        };
        let store = self.source.session.block_store();
        let op = BlockListMirror::scroll_to_px(
            &store.lock(),
            self.live_history_rows(&frame),
            frame_content_rows(&frame),
            (self.content_cols(), cell.height_px, self.settings.pad_rows),
            target,
        );
        self.block_list.scrollbar.0 = target.min(self.block_list.scrollbar.1);
        self.update_viewport();
        ScrollOutcome::List(op)
    }

    pub(crate) fn jump_to_block(&mut self, direction: i8) -> ScrollOutcome {
        let Some(cell) = self.cell_metrics else {
            return ScrollOutcome::Ignored;
        };
        let store = self.source.session.block_store();
        let target = nav_item_top(
            &store.lock(),
            self.content_cols(),
            cell.height_px,
            self.settings.pad_rows,
            self.block_list.scrollbar.0,
            direction,
        );
        target.map_or(ScrollOutcome::Ignored, |target| self.scroll_list_to(target))
    }
}
