use gpui::{FollowMode, ListAlignment, ListOffset, ListState, px};

use crate::terminal_tab::pane_model::list_mirror::ListOp;

pub(crate) struct BlockListState {
    pub list: ListState,
    pub scroll_handler_set: bool,
}

impl BlockListState {
    pub(crate) fn new(alignment: ListAlignment) -> Self {
        let list = ListState::new(1, alignment, px(240.0));

        list.set_follow_mode(FollowMode::Tail);

        Self {
            list,
            scroll_handler_set: false,
        }
    }

    pub(crate) fn apply(&mut self, op: ListOp) {
        match op {
            ListOp::Reset(count) => self.list.reset(count),
            ListOp::Splice(range, count) => self.list.splice(range, count),
            ListOp::RemeasureAll => self.list.remeasure(),
            ListOp::Remeasure(range) => self.list.remeasure_items(range),

            ListOp::ScrollTo(pos) => self.list.scroll_to(ListOffset {
                item_ix: pos.item_ix,
                offset_in_item: px(pos.offset_px),
            }),

            ListOp::ScrollToEnd => self.list.scroll_to_end(),
        }
    }
}

pub(crate) fn block_list_alignment(fixed_bottom: bool) -> ListAlignment {
    if fixed_bottom {
        ListAlignment::Bottom
    } else {
        ListAlignment::Top
    }
}
