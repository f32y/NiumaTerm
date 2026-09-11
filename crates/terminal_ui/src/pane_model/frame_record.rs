use crate::block_list::chrome::offset_frozen_chrome;
use crate::block_list::{FrozenItemChrome, FrozenView};
use crate::pane_model::PaneController;

/// The small hit-test record retained after the element keeps its shaped rows.
pub(crate) struct FrameRecord {
    rows: Vec<(f32, usize, usize, u32)>,
    separators: Vec<f32>,
    chrome: Vec<FrozenItemChrome>,
    pub active_top: Option<f32>,
}

impl FrameRecord {
    pub(crate) fn from_view(view: &FrozenView, item_top: f32) -> Self {
        Self {
            rows: view
                .rows
                .iter()
                .map(|row| (item_top + row.y, row.item, row.row, row.cell_count))
                .collect(),
            separators: view.separators.iter().map(|y| item_top + y).collect(),
            chrome: view
                .items_chrome
                .iter()
                .cloned()
                .map(|chrome| offset_frozen_chrome(chrome, item_top))
                .collect(),
            active_top: None,
        }
    }

    pub(crate) fn push_chrome(&mut self, chrome: FrozenItemChrome, item_top: f32) {
        self.chrome.push(offset_frozen_chrome(chrome, item_top));
    }
}

impl PaneController {
    pub(crate) fn record_frame(&mut self, record: FrameRecord) {
        for (y, item, row, cols) in record.rows {
            self.frozen.push_row(y, item, row, cols);
        }
        for y in record.separators {
            self.frozen.push_separator(y);
        }
        for chrome in record.chrome {
            self.frozen.push_chrome(chrome, 0.0);
        }
        if let Some(top) = record.active_top {
            self.frozen.set_active_top(top);
            self.update_viewport();
        }
    }
}
