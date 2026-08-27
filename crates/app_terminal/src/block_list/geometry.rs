use crate::block_list;
use crate::block_list::*;

/// Blank rows above and below each item's content: one full cell row on each
/// side, with the separator rule on the item's top edge — so adjacent blocks
/// read as content / blank / rule / blank / content. Compact presentation
/// (Command Blocks off) passes `pad_rows = 0.0` through the geometry
/// functions instead, packing rows contiguously like a classic grid.
pub(crate) const ITEM_PAD_ROWS: f32 = 1.0;

/// Row count of one item — the cached engine row count (already wrapped at
/// the current width; the engine reflows blocks eagerly on resize).
pub(crate) fn item_rows(item: &BlockItem, _cols: u32) -> u32 {
    item.engine_rows().min(u32::MAX as usize) as u32
}

/// Pixel height of one item: content rows plus `pad_rows` blank rows above
/// and below. Empty items (empty commands never freeze, but a stale cache can
/// briefly report 0) are invisible — no rows, no pads.
pub(crate) fn item_px(item: &BlockItem, cols: u32, cell_h: f32, pad_rows: f32) -> f32 {
    match item_rows(item, cols) {
        0 => 0.0,
        rows => (rows as f32 + 2.0 * pad_rows) * cell_h,
    }
}

/// Pixel height of the live item: pads + the active grid's scrolled-out
/// history rows + the live grid's content rows. Shared by the item element's
/// layout and the render metrics so the two cannot drift.
pub(crate) fn live_item_px(history_rows: u64, live_rows: usize, cell_h: f32, pad_rows: f32) -> f32 {
    history_rows as f32 * cell_h + (live_rows as f32 + 2.0 * pad_rows) * cell_h
}

/// The item-local row range intersecting the window viewport (plus the list's
/// overdraw margin), so a huge block materializes only its visible rows
/// while reading only the visible row range.
pub(crate) fn visible_rows(
    item_top_in_window: f32,
    item_rows: usize,
    viewport_h: f32,
    cell_h: f32,
    pad_rows: f32,
) -> ops::Range<usize> {
    const OVERDRAW: f32 = 260.0;

    let pad = pad_rows * cell_h;
    let visible_top = (-item_top_in_window - OVERDRAW).max(0.0);
    let visible_bottom = viewport_h - item_top_in_window + OVERDRAW;
    if visible_bottom <= 0.0 || cell_h <= 0.0 {
        return 0..0;
    }

    let first = ((visible_top - pad) / cell_h).floor().max(0.0) as usize;
    let last = (((visible_bottom - pad) / cell_h).ceil().max(0.0) as usize).min(item_rows);

    first.min(last)..last
}

/// The list-top y of the previous (`direction < 0`) or next item relative to
/// the current scroll position; `None` at the edges.
pub(crate) fn nav_item_top(
    store: &BlockStore,
    cols: u32,
    cell_h: f32,
    pad_rows: f32,
    from_px: f32,
    direction: i8,
) -> Option<f32> {
    let mut tops = Vec::new();
    let mut y = 0.0f32;

    for item in store.items() {
        if item_rows(item, cols) > 0 {
            tops.push(y);
        }
        y += item_px(item, cols, cell_h, pad_rows);
    }

    // Half-pixel slop so the item currently at the top does not match itself.
    if direction < 0 {
        tops.into_iter().rev().find(|t| *t < from_px - 0.5)
    } else {
        tops.into_iter().find(|t| *t > from_px + 0.5)
    }
}

/// Blank rows around each block for the current presentation: chrome shows
/// one pad row above and below; compact (Command Blocks off) packs block rows
/// contiguously like a classic grid. Every block-list geometry consumer must
/// use this one value per frame so heights, hit-testing, and scroll math agree.
pub(crate) fn block_pad_rows(cx: &App) -> f32 {
    if cx.global::<TerminalSettings>().command_blocks {
        block_list::ITEM_PAD_ROWS
    } else {
        0.0
    }
}

pub(crate) fn block_list_alignment(fixed_bottom: bool) -> ListAlignment {
    if fixed_bottom {
        ListAlignment::Bottom
    } else {
        ListAlignment::Top
    }
}

/// Element-local top of the live grid: frozen items, then the live item's
/// top pad and tail rows. Computed from the per-frame metrics so it stays
/// valid even when the live item is outside List's prepaint overdraw.
pub(crate) fn block_list_active_top_px(
    frozen_px: f32,
    tail_px: f32,
    cell_h: f32,
    pad_rows: f32,
    scroll_top: f32,
) -> f32 {
    (frozen_px + pad_rows * cell_h + tail_px - scroll_top).max(0.0)
}
