//! Renders history as a real vertical list: frozen items above — each a
//! finished engine block read directly through a refcounted `BlockRef` —
//! plus one live item for the current engine viewport (with the active
//! grid's scrollback rows rendered above it while a command runs).
//! Scrolling is pure UI state over the list — the engine viewport stays
//! pinned at the bottom.

mod chrome;
mod geometry;
mod images;
mod paint;
mod reconcile;
mod rows;
mod selection;

use std::{collections, iter, ops, sync, time};

use gpui::{
    App, Bounds, FollowMode, Hsla, ListAlignment, ListOffset, ListState, Pixels, Rgba, ShapedLine,
    SharedString, TextAlign, TextRun, Window, fill, point, px, rgb, rgba, size,
};
use nmt_terminal::block_store::{BlockItem, BlockStore, SegmentMeta};
use nmt_terminal::ghostty::{
    BlockHandle, BlockRef, CellText, CellWide, Palette, PlacementScreenPos, SnapshotStyle,
    Underline,
};
use nmt_terminal::grid_emit::row_selection_for;
use nmt_terminal::selection::SelectionRange;
use nmt_terminal::terminal::square::Wide;

use crate::terminal;
#[cfg(test)]
use crate::terminal::block_list::chrome::item_header;
pub(crate) use crate::terminal::block_list::chrome::{
    FrozenItemChrome, block_list_live_chrome, live_chrome, offset_frozen_chrome,
    paint_frozen_chrome, paint_frozen_separators,
};
#[cfg(test)]
use crate::terminal::block_list::geometry::item_rows;
pub(crate) use crate::terminal::block_list::geometry::{
    ITEM_PAD_ROWS, block_list_active_top_px, block_list_alignment, block_pad_rows, item_px,
    live_item_px, nav_item_top, visible_rows,
};
pub(crate) use crate::terminal::block_list::images::{FrozenImage, frozen_block_images};
pub(crate) use crate::terminal::block_list::paint::{paint_frozen, shape_frozen_rows};
pub(crate) use crate::terminal::block_list::reconcile::{
    BlockListMeasureKey, BlockListState, ListReconcile, RemeasureScope, block_list_render_metrics,
    plan_list_reconcile, plan_remeasure, shift_selected_item_for_eviction,
};
#[cfg(test)]
use crate::terminal::block_list::rows::HandleItemInfo;
pub(crate) use crate::terminal::block_list::rows::{
    EngineRowBuilder, frozen_block_view, handle_item_info, live_history_view,
};
pub(crate) use crate::terminal::block_list::selection::{
    BlockListPoint, FrozenHitInfo, FrozenPoint, frozen_selection_pieces,
};
use crate::terminal::frame::{
    LineBuilder, StyleRun, TerminalCell, TerminalColor, TerminalLine, theme_default_foreground,
    theme_selection_background,
};
use crate::terminal::layout::truncate_command;
use crate::terminal::paint_text::{
    block_separator_bounds, paint_glyph_rows, paint_line_backgrounds_at, shape_lines,
};
use crate::terminal::session::InFlightBlock;
use crate::terminal::settings::TerminalSettings;
use crate::terminal::theme::{
    BLOCK_FAILURE_COLOR, BLOCK_GUTTER_GAP, BLOCK_GUTTER_WIDTH, BLOCK_INPUT_COLOR,
    BLOCK_RUNNING_COLOR, BLOCK_SELECTED_TINT, BLOCK_SUCCESS_COLOR, SEPARATOR_COLOR,
};

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
