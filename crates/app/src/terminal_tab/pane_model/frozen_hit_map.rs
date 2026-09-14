use crate::terminal_tab::block_list::chrome::offset_frozen_chrome;
use crate::terminal_tab::block_list::{BlockListPoint, FrozenItemChrome, FrozenPoint};

/// Rows, chrome and separators recorded during one prepaint for later
/// pointer mapping and painting. Persistent selection is stored separately.
#[derive(Default)]
pub(crate) struct FrozenHitMap {
    /// Hit-test data recorded from the last native list prepaint.
    hit: FrozenHitInfo,

    /// Visible frozen item chrome recorded from native list item bounds.
    chrome: Vec<FrozenItemChrome>,

    /// Visible separator y positions, painted outside GPUI List's content mask.
    separators: Vec<f32>,
}

impl FrozenHitMap {
    /// Drop last frame's record; the prepaint that follows rebuilds it.
    pub(crate) fn begin_frame(&mut self, active_top: f32) {
        self.hit.clear();

        self.hit.set_active_top(active_top);

        self.chrome.clear();

        self.separators.clear();
    }

    pub(crate) fn set_active_top(&mut self, active_top: f32) {
        self.hit.set_active_top(active_top);
    }

    /// Element-local top of the live grid; rows at or below it belong to the
    /// engine viewport rather than to the frozen region.
    pub(crate) fn active_top(&self) -> f32 {
        self.hit.active_top
    }

    pub(crate) fn push_separator(&mut self, y: f32) {
        self.separators.push(y);
    }

    pub(crate) fn push_row(&mut self, y: f32, item: usize, row: usize, cell_count: u32) {
        self.hit.push_row(y, item, row, cell_count);
    }

    pub(crate) fn push_chrome(&mut self, chrome: FrozenItemChrome, item_top: f32) {
        self.chrome.push(offset_frozen_chrome(chrome, item_top));
    }

    pub(crate) fn chrome(&self) -> &[FrozenItemChrome] {
        &self.chrome
    }

    pub(crate) fn separators(&self) -> &[f32] {
        &self.separators
    }

    /// The content-local y of one visible row; `None` when it is scrolled out
    /// of view.
    pub(crate) fn row_top(&self, item: usize, row: usize) -> Option<f32> {
        self.hit.row_top(item, row)
    }

    pub(crate) fn hit_test(
        &self,
        x: f32,
        y: f32,
        cell_width: f32,
        cell_height: f32,
        cols: u32,
        pad_rows: f32,
    ) -> Option<BlockListPoint> {
        self.hit
            .hit_test(x, y, cell_width, cell_height, cols, pad_rows)
    }
}

/// Pane-side hit-test data for rows rendered above the active grid (small
/// copy; the full view moves into the element).
#[derive(Default, Clone)]
pub(crate) struct FrozenHitInfo {
    /// `(y, item, row, cell_count)` per visible block or live-history row;
    /// `usize::MAX` marks a live SCREEN row because it cannot be a list index.
    rows: Vec<(f32, usize, usize, u32)>,

    pub active_top: f32,
}

impl FrozenHitInfo {
    pub(crate) fn clear(&mut self) {
        self.rows.clear();

        self.active_top = 0.0;
    }

    pub(crate) fn push_row(&mut self, y: f32, item: usize, row: usize, cell_count: u32) {
        self.rows.push((y, item, row, cell_count));
    }

    pub(crate) fn set_active_top(&mut self, active_top: f32) {
        self.active_top = active_top;
    }

    /// The content-local y of one visible row (`usize::MAX` item = a live
    /// SCREEN row); `None` when the row is scrolled out of view. Link-hover
    /// underlines use this to place rects on frozen rows.
    pub(crate) fn row_top(&self, item: usize, row: usize) -> Option<f32> {
        self.rows
            .iter()
            .find(|(_, i, r, _)| *i == item && *r == row)
            .map(|(y, ..)| *y)
    }

    /// Map an element-local pixel position to a frozen point. `None` above
    /// the first visible row; positions in inter-item gaps resolve to the
    /// nearest row above (drag comfort).
    pub(crate) fn hit_test(
        &self,
        x: f32,
        y: f32,
        cell_w: f32,
        cell_h: f32,
        cols: u32,
        pad_rows: f32,
    ) -> Option<BlockListPoint> {
        let (_, item, row, cell_count) = *self
            .rows
            .iter()
            .take_while(|(ry, ..)| *ry <= y)
            .last()
            .filter(|(ry, ..)| y < ry + cell_h * (1.0 + pad_rows))?;

        let local = (x / cell_w.max(1.0)).floor().max(0.0) as u32;
        let col = local.min(cols.saturating_sub(1)).min(cell_count);

        if item == usize::MAX {
            return Some(BlockListPoint::LiveHistory {
                row: row.min(u32::MAX as usize) as u32,
                col: col.min(u16::MAX as u32) as u16,
            });
        }

        Some(BlockListPoint::Frozen(FrozenPoint {
            item,
            line: row,
            col,
        }))
    }
}
