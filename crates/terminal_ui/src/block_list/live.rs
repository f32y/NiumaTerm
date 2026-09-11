use nmt_terminal::session::InFlightBlock;

use crate::block_list::{FrozenItemChrome, block_list_live_chrome};

pub(crate) struct LiveItemState {
    pub index: usize,
    pub in_flight: Option<InFlightBlock>,
    pub has_open_prompt: bool,
    pub selected_item: Option<usize>,
}

pub(crate) struct LiveItemLayout {
    pub active_top: f32,
    pub active_height: f32,
    pub chrome: Option<FrozenItemChrome>,
}

impl LiveItemState {
    pub(crate) fn layout(
        &self,
        history_height: f32,
        live_rows: usize,
        cell_height: f32,
        pad_rows: f32,
    ) -> LiveItemLayout {
        let active_height = live_rows as f32 * cell_height;
        let chrome = block_list_live_chrome(
            self.index,
            live_rows,
            cell_height,
            self.in_flight.as_ref(),
            self.has_open_prompt,
            self.selected_item == Some(self.index),
        )
        .map(|mut chrome| {
            chrome.bottom = history_height + active_height + pad_rows * cell_height;
            chrome.header_y = history_height;
            chrome
        });
        LiveItemLayout {
            active_top: history_height,
            active_height,
            chrome,
        }
    }
}
