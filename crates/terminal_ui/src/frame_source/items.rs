use std::collections::{HashMap, HashSet};

use nmt_terminal::session::BlockPoint;
use nmt_terminal::session::page::PAGE_ROWS;

use crate::block_list::chrome::DurationLabels;
use crate::block_list::{self, FrozenView};
use crate::frame::TerminalColor;
use crate::frame_source::TerminalFrameSource;

pub(crate) struct ItemViewport {
    pub top: f32,
    pub height: f32,
    pub cell_height: f32,
    pub pad_rows: f32,
}

impl TerminalFrameSource {
    pub(crate) fn frozen_block_view(
        &self,
        item_idx: usize,
        viewport: &ItemViewport,
        selection: Option<(BlockPoint, BlockPoint)>,
        selected_item: Option<usize>,
        labels: &DurationLabels,
        foreground: TerminalColor,
    ) -> FrozenView {
        let Some((info, handle)) = self
            .session
            .block_item(item_idx)
            .and_then(|item| block_list::handle_item_info(&item, labels).zip(item.handle()))
        else {
            return FrozenView::default();
        };
        let visible = block_list::visible_rows(
            viewport.top,
            info.rows,
            viewport.height,
            viewport.cell_height,
            viewport.pad_rows,
        );
        let first = visible.start / PAGE_ROWS * PAGE_ROWS;
        let pages: Vec<_> = (first..visible.end)
            .step_by(PAGE_ROWS)
            .filter_map(|row| self.session.block_page(handle, row))
            .collect();
        let mut view = block_list::frozen_block_view(
            &pages,
            &info,
            item_idx,
            visible.clone(),
            viewport.cell_height,
            viewport.pad_rows,
            selection,
            selected_item,
            foreground,
        );
        let mut seen = HashSet::new();
        let mut placements = Vec::new();
        let mut generations = HashMap::new();
        for page in &pages {
            for placement in &page.placements {
                if seen.insert((
                    placement.image_id,
                    placement.placement_id,
                    placement.screen_col,
                    placement.screen_row,
                )) {
                    placements.push(*placement);
                }
                if let Some(generation) = self.frozen_image(page, placement.image_id) {
                    generations.insert(placement.image_id, generation);
                }
            }
        }
        view.images = block_list::frozen_block_images(
            &placements,
            &generations,
            &visible,
            viewport.cell_height,
            viewport.pad_rows,
        );
        view
    }

    pub(crate) fn live_history_view(
        &self,
        history_rows: u64,
        cols: u32,
        viewport: &ItemViewport,
        foreground: TerminalColor,
    ) -> FrozenView {
        let visible = block_list::visible_rows(
            viewport.top,
            history_rows.min(usize::MAX as u64) as usize,
            viewport.height,
            viewport.cell_height,
            viewport.pad_rows,
        );
        let lines = self.live_history_lines(visible.start as u64..visible.end as u64, foreground);
        let selection = self.session.selection_screen_range_in(&self.snapshot);
        block_list::live_history_view(
            lines,
            history_rows,
            cols,
            viewport.cell_height,
            viewport.pad_rows,
            selection,
        )
    }
}
