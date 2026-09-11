//! Renders history as a real vertical list: frozen items above — each a
//! finished engine block read directly through a refcounted `BlockRef` —
//! plus one live item for the current engine viewport (with the active
//! grid's scrollback rows rendered above it while a command runs).
//! Scrolling is pure UI state over the list — the engine viewport stays
//! pinned at the bottom.

pub(crate) use nmt_terminal::session::BlockPoint as FrozenPoint;

pub(crate) mod chrome;
mod geometry;
mod images;
pub(crate) mod live;
pub(crate) mod reconcile;
mod rows;
mod selection;

pub(crate) use crate::block_list::chrome::{FrozenItemChrome, block_list_live_chrome, live_chrome};
pub(crate) use crate::block_list::geometry::{
    ITEM_PAD_ROWS, block_list_active_top_px, item_px, live_item_px, nav_item_top, visible_rows,
};
pub(crate) use crate::block_list::images::{FrozenImage, frozen_block_images};
pub(crate) use crate::block_list::reconcile::{
    BlockListMeasureKey, ListReconcile, RemeasureScope, block_list_render_metrics,
    plan_list_reconcile,
};
pub(crate) use crate::block_list::rows::{
    EngineRowBuilder, frozen_block_view, handle_item_info, live_history_view,
};
pub(crate) use crate::block_list::selection::BlockListPoint;
use crate::frame::TerminalLine;

/// One visible frozen row, positioned in element-local pixels.
pub(crate) struct FrozenRow {
    pub y: f32,
    pub line: TerminalLine,

    /// Source position: store item / physical block row. Engine blocks are
    /// already wrapped at the current width, so a row IS a visual row.
    pub item: usize,

    pub row: usize,

    /// Source row width, for hit-testing column clamps.
    pub cell_count: u32,

    /// Selected column span (row-local, end exclusive).
    pub selected: Option<(u16, u16)>,

    /// Shaped-line cache key: `(block_id, generation, row)` for block rows
    /// (immutable per generation, so the layout caches across frames without
    /// hashing row text). `None` → hash the text (live history rows).
    pub shape_key: Option<u64>,
}

/// Item-local frozen rows and chrome for one list item. GPUI's native list
/// decides which items are visible and where they sit.
#[derive(Default)]
pub(crate) struct FrozenView {
    pub rows: Vec<FrozenRow>,

    /// Chrome for each visible non-empty item.
    pub items_chrome: Vec<FrozenItemChrome>,

    /// Separator rule positions (item boundaries inside the visible window).
    pub separators: Vec<f32>,

    /// Frozen Kitty image bands in this item.
    pub images: Vec<FrozenImage>,

    /// Where the active region (live engine viewport) starts.
    pub active_top: f32,
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod layout_tests;
