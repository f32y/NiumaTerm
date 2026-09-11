use crate::block_list::chrome::offset_frozen_chrome;
use crate::block_list::live::LiveItemLayout;
use crate::block_list::{FrozenItemChrome, FrozenView};

/// The small hit-test record retained after the element keeps its shaped rows.
pub(crate) struct FrameRecord {
    pub(super) rows: Vec<(f32, usize, usize, u32)>,
    pub(super) separators: Vec<f32>,
    pub(super) chrome: Vec<FrozenItemChrome>,
    pub(super) active_top: Option<f32>,
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

    pub(crate) fn from_live_view(
        view: &FrozenView,
        layout: &LiveItemLayout,
        item_top: f32,
    ) -> Self {
        let mut record = Self::from_view(view, item_top);

        record.active_top = Some(item_top + layout.active_top);

        if let Some(chrome) = &layout.chrome {
            record
                .chrome
                .push(offset_frozen_chrome(chrome.clone(), item_top));
        }

        record
    }
}
