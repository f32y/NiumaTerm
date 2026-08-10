//! Renders history as a real vertical list: frozen items above — each a
//! finished engine block read directly through a refcounted `BlockRef` —
//! plus one live item for the current engine viewport (with the active
//! grid's scrollback rows rendered above it while a command runs).
//! Scrolling is pure UI state over the list — the engine viewport stays
//! pinned at the bottom.

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
use crate::terminal::frame::{
    LineBuilder, StyleRun, TerminalCell, TerminalColor, TerminalLine, theme_default_foreground,
    theme_selection_background,
};
use crate::terminal::session::InFlightBlock;
use crate::terminal::theme::{
    BLOCK_FAILURE_COLOR, BLOCK_GUTTER_GAP, BLOCK_GUTTER_WIDTH, BLOCK_INPUT_COLOR,
    BLOCK_RUNNING_COLOR, BLOCK_SELECTED_TINT, BLOCK_SUCCESS_COLOR, SEPARATOR_COLOR,
};
use crate::ui::AppSettings;

/// Blank rows above and below each item's content: one full cell row on each
/// side, with the separator rule on the item's top edge — so adjacent blocks
/// read as content / blank / rule / blank / content. Compact presentation
/// (Command Blocks off) passes `pad_rows = 0.0` through the geometry
/// functions instead, packing rows contiguously like a classic grid.
pub(crate) const ITEM_PAD_ROWS: f32 = 1.0;
pub(crate) struct BlockListState {
    /// Native GPUI list state for block-split rendering.
    pub list: ListState,
    /// Last item count mirrored into `list`.
    pub item_count: usize,
    /// Last store eviction counter mirrored into `list`.
    pub evicted_items: u64,
    /// The native list scroll callback is stable for the pane; install once.
    pub scroll_handler_set: bool,
    /// Pixel mirror of native list scroll: `(scroll_top, max_scroll)`.
    pub scrollbar: (f32, f32),
    /// Element-local top of the live grid, even outside list prepaint overdraw.
    pub active_top: f32,
}

impl BlockListState {
    pub(crate) fn new(alignment: ListAlignment) -> Self {
        let list = ListState::new(1, alignment, px(240.0));
        list.set_follow_mode(FollowMode::Tail);
        Self {
            list,
            item_count: 1,
            evicted_items: 0,
            scroll_handler_set: false,
            scrollbar: (0.0, 0.0),
            active_top: 0.0,
        }
    }
}

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

/// A position in the frozen history: store item, physical block row, column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct FrozenPoint {
    pub item: usize,
    pub line: usize,
    pub col: u32,
}

/// A selectable row rendered by the block list. Finished blocks use their
/// immutable block coordinates; the active block's history keeps the engine's
/// absolute SCREEN row so selection and copy remain owned by Ghostty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BlockListPoint {
    Frozen(FrozenPoint),
    LiveHistory { row: u32, col: u16 },
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

/// Chrome of one visible item: gutter accent, right-aligned header,
/// selection state. Element coords (scroll already subtracted); may extend
/// past the visible window — the paint's content mask clips.
#[derive(Clone)]
pub(crate) struct FrozenItemChrome {
    pub item: usize,
    pub top: f32,
    pub bottom: f32,
    pub header_y: f32,
    /// 0xRRGGBB, keyed off the exit code (running/success/failure).
    pub accent: u32,
    /// "cmd · ✓ 1.2s" / "cmd · ✗ 127"; `None` when no command is known.
    pub header: Option<String>,
    pub selected: bool,
}

/// A frozen Kitty image band positioned inside one block-list item: one
/// cell-row-high slice of a placement, with the generation shared
/// by `Arc` from the store's lazy `(block_id, image_id)` cache.
pub(crate) struct FrozenImage {
    pub generation: sync::Arc<terminal::graphics::ImageGeneration>,
    pub z: i32,
    /// Element-local y of the row's top edge.
    pub y: f32,
    /// Column within the row, and cell width of the band.
    pub col: u32,
    pub width: u32,
    /// Normalized source rectangle `[u0, v0, u1, v1]` into the full image.
    pub source: [f32; 4],
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

/// Gutter/header accent for a frozen item, keyed off the exit code.
fn item_accent(meta: &SegmentMeta) -> u32 {
    match meta.exit_code {
        None => BLOCK_RUNNING_COLOR,
        Some(0) => BLOCK_SUCCESS_COLOR,
        Some(_) => BLOCK_FAILURE_COLOR,
    }
}

/// Header label of a frozen item: truncated command + status/duration.
/// `None` without a command (nothing meaningful to show).
fn item_header(meta: &SegmentMeta) -> Option<String> {
    let command = meta.command.as_deref()?;
    let ended_at = meta.ended_at?;
    let duration = meta
        .started_at
        .and_then(|started_at| ended_at.duration_since(started_at).ok())
        .map(format_duration);

    let status = match (meta.exit_code, duration) {
        (Some(0), Some(d)) => format!("✓ {d}"),
        (Some(0), None) => "✓".to_string(),
        (Some(code), Some(d)) => format!("✗ {code} · {d}"),
        (Some(code), None) => format!("✗ {code}"),
        (None, Some(d)) => format!("? · {d}"),
        (None, None) => "?".to_string(),
    };

    Some(command_header(command, &status))
}

fn command_header(command: &str, status: &str) -> String {
    format!(
        "{} · {status}",
        terminal::terminal_view::truncate_command(command, 32)
    )
}

/// Chrome of the live item: a running command uses the running accent, while
/// the idle input region uses the input accent. Headers appear only after the
/// item is finished. `rows == 0` → invisible.
pub(crate) fn live_chrome(
    item: usize,
    rows: usize,
    cell_h: f32,
    running: bool,
    selected: bool,
) -> Option<FrozenItemChrome> {
    if rows == 0 {
        return None;
    }

    let accent = if running {
        BLOCK_RUNNING_COLOR
    } else {
        BLOCK_INPUT_COLOR
    };

    Some(FrozenItemChrome {
        item,
        top: 0.0,
        bottom: rows as f32 * cell_h,
        header_y: 0.0,
        accent,
        header: None,
        selected,
    })
}

/// `1.2s` / `815ms` / `2m05s` — the header's duration label.
pub(crate) fn format_duration(d: time::Duration) -> String {
    let secs = d.as_secs();

    if secs >= 60 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else if secs >= 1 {
        format!("{:.1}s", d.as_secs_f32())
    } else {
        format!("{}ms", d.as_millis())
    }
}

/// Builds one display line from an engine row visit (frozen-block row or
/// active-grid history row): every column contributes a char (gaps become
/// NBSP), spacer cells are dropped. Display conventions (wide placeholder,
/// run merging) come from the shared `LineBuilder`, so frozen rows shape and
/// paint exactly like live ones.
#[derive(Default)]
pub(crate) struct EngineRowBuilder {
    line: LineBuilder,
    col: u16,
}

impl EngineRowBuilder {
    pub(crate) fn push(
        &mut self,
        x: u16,
        cell_text: CellText,
        wide: CellWide,
        style: &SnapshotStyle,
        default_fg: TerminalColor,
    ) {
        use nmt_terminal::ghostty::CellWide;

        match wide {
            CellWide::SpacerTail | CellWide::SpacerHead => return,
            CellWide::Narrow | CellWide::Wide => {}
        }

        let default_style = StyleRun {
            len: 0,
            fg: default_fg,
            bold: false,
            italic: false,
            underline: false,
            strikethrough: false,
        };

        while self.col < x {
            self.line
                .push_segment(iter::once('\u{00a0}'), default_style, false);
            self.col += 1;
        }

        let (fg, bg) = if style.inverse {
            (
                style.bg.unwrap_or(default_fg),
                Some(style.fg.unwrap_or(default_fg)),
            )
        } else {
            (style.fg.unwrap_or(default_fg), style.bg)
        };

        let is_wide = wide == CellWide::Wide;
        let display: String = if cell_text.is_empty() {
            "\u{00a0}".into()
        } else {
            cell_text.replace([' ', '\t'], "\u{00a0}")
        };

        self.line.push_segment(
            display.chars(),
            StyleRun {
                len: 0,
                fg,
                bold: style.bold,
                italic: style.italic,
                underline: style.underline != Underline::None,
                strikethrough: style.strikethrough,
            },
            is_wide,
        );

        self.line.push_cell(TerminalCell {
            col: x,
            ch: cell_text.chars().next().unwrap_or('\0'),
            style_id: 0,
            background: bg,
            wide: if is_wide { Wide::Wide } else { Wide::Narrow },
            extras: cell_text.chars().skip(1).collect(),
            has_cursor: false,
        });

        self.col = x + if is_wide { 2 } else { 1 };
    }

    pub(crate) fn finish(self) -> TerminalLine {
        self.line.finish()
    }
}

/// Chrome inputs of a block item, cloneable out of the store lock —
/// prepaint acquires the engine `BlockRef` afterwards, and the store and
/// engine locks must never nest (surface lock discipline).
#[derive(Clone)]
pub(crate) struct HandleItemInfo {
    pub handle: BlockHandle,
    /// Cached engine row count — the layout height source.
    pub rows: usize,
    pub accent: u32,
    pub header: Option<String>,
}

pub(crate) fn handle_item_info(item: &BlockItem) -> Option<HandleItemInfo> {
    let handle = item.handle()?;

    Some(HandleItemInfo {
        handle,
        rows: item.engine_rows(),
        accent: item_accent(&item.meta),
        header: item_header(&item.meta),
    })
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

/// The frozen view of an engine-block item: physical rows read through the
/// acquired [`BlockRef`], only for the visible range. Row `r`
/// keeps its item-local y even when earlier rows are skipped, so geometry
/// matches `item_px` exactly. `block = None` (stale handle / mid-reflow)
/// renders chrome at the cached height with no rows — content returns next
/// frame.
#[allow(clippy::too_many_arguments)]
pub(crate) fn frozen_block_view(
    block: Option<(&BlockRef, &Palette)>,
    info: &HandleItemInfo,
    item_idx: usize,
    visible: ops::Range<usize>,
    cell_h: f32,
    pad_rows: f32,
    selection: Option<(FrozenPoint, FrozenPoint)>,
    selected_item: Option<usize>,
) -> FrozenView {
    let default_fg = theme_default_foreground();
    let selection = selection.map(|(a, b)| if a <= b { (a, b) } else { (b, a) });
    let rows = info.rows;
    let pad = pad_rows * cell_h;

    let mut view = FrozenView {
        rows: Vec::new(),
        items_chrome: Vec::new(),
        separators: Vec::new(),
        images: Vec::new(),
        active_top: (rows as f32 + 2.0 * pad_rows) * cell_h,
    };

    if rows == 0 {
        return view;
    }

    // Every block opens with a rule on its top edge; the neighbors' pad rows
    // give it a blank line on each side.
    view.separators.push(0.0);
    view.items_chrome.push(FrozenItemChrome {
        item: item_idx,
        top: 0.0,
        bottom: view.active_top,
        header_y: pad,
        accent: info.accent,
        header: info.header.clone(),
        selected: selected_item == Some(item_idx),
    });

    let Some((block, palette)) = block else {
        return view;
    };

    let handle = block.handle();
    let cols = u32::from(block.cols());

    // The snapshot is the reading truth; the cached row count is the layout
    // truth. Read only rows both agree on (a lagging sync converges next
    // frame).
    let read_rows = rows.min(block.row_count());

    for row in visible.start..visible.end.min(read_rows) {
        let mut builder = EngineRowBuilder::default();
        let ok = block
            .read_row_visit(row, palette, |x, t, w, s| {
                builder.push(x, t, w, &s, default_fg)
            })
            .ok()
            .flatten()
            .is_some();

        if !ok {
            break;
        }

        let line = builder.finish();
        let selected =
            selected_span(selection, item_idx, row, cols).map(|span| expand_wide_span(&line, span));

        view.rows.push(FrozenRow {
            y: pad + row as f32 * cell_h,
            line,
            item: item_idx,
            row,
            cell_count: cols,
            selected,
            shape_key: Some(block_row_shape_key(handle, row)),
        });
    }
    view
}

/// Frozen Kitty images of an engine-block item, mapped to the visible row
/// range: each placement contributes one cell-row band per
/// visible row it spans, with the source rectangle subdivided vertically
/// (boundary-difference math — no cumulative rounding gaps). Placement
/// positions are block-relative rows straight from the engine; generations
/// come from the store's `(block_id, image_id)` lazy cache.
pub(crate) fn frozen_block_images(
    placements: &[PlacementScreenPos],
    generations: &collections::HashMap<u32, sync::Arc<terminal::graphics::ImageGeneration>>,
    visible: &ops::Range<usize>,
    cell_h: f32,
    pad_rows: f32,
) -> Vec<FrozenImage> {
    let pad = pad_rows * cell_h;
    let mut out = Vec::new();

    for p in placements {
        if p.grid_rows == 0 || p.grid_cols == 0 {
            continue;
        }

        let Some(generation) = generations.get(&p.image_id) else {
            continue; // pixels unavailable (evicted mid-read); retry next frame
        };

        let size = generation.image().size(0);
        let (iw, ih) = (size.width.0.max(0) as f32, size.height.0.max(0) as f32);

        if iw <= 0.0 || ih <= 0.0 {
            continue;
        }

        for k in 0..p.grid_rows {
            let row = p.screen_row as usize + k as usize;
            if !visible.contains(&row) {
                continue;
            }

            let sy0 = p.source_y + p.source_height.saturating_mul(k) / p.grid_rows;
            let sy1 = p.source_y + p.source_height.saturating_mul(k + 1) / p.grid_rows;

            out.push(FrozenImage {
                generation: generation.clone(),
                z: p.z,
                y: pad + row as f32 * cell_h,
                col: p.screen_col,
                width: p.grid_cols,
                source: [
                    p.source_x as f32 / iw,
                    sy0 as f32 / ih,
                    ((p.source_x + p.source_width) as f32 / iw).min(1.0),
                    (sy1 as f32 / ih).min(1.0),
                ],
            });
        }
    }
    out
}

/// Shaped-line cache key for an engine-block row: `(block_id, generation,
/// row)`. Content is immutable per generation, so the layout
/// caches across frames without hashing the row text.
fn block_row_shape_key(handle: BlockHandle, row: usize) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = collections::hash_map::DefaultHasher::new();

    (handle.id, handle.generation, row).hash(&mut hasher);

    hasher.finish()
}

/// The live item's scrolled-up history: active-grid scrollback rows read as
/// physical lines rendered above the live grid. Rows carry
/// an out-of-band item index that the hit map converts back to their absolute
/// SCREEN row; selection remains owned by the engine rather than BlockStore.
pub(crate) fn live_history_view(
    lines: Vec<(u64, TerminalLine)>,
    total_rows: u64,
    cols: u32,
    cell_h: f32,
    pad_rows: f32,
    selection: Option<SelectionRange>,
) -> FrozenView {
    let pad = pad_rows * cell_h;
    let mut view = FrozenView {
        rows: Vec::new(),
        items_chrome: Vec::new(),
        separators: vec![0.0],
        images: Vec::new(),
        active_top: pad + total_rows as f32 * cell_h,
    };

    for (row, line) in lines {
        let selected = usize::try_from(row)
            .ok()
            .filter(|row| *row <= i32::MAX as usize)
            .and_then(|row| row_selection_for(selection, row, cols as usize))
            .map(|span| (span.lo, span.hi.saturating_add(1)))
            .map(|span| expand_wide_span(&line, span));

        view.rows.push(FrozenRow {
            y: pad + row as f32 * cell_h,
            line,
            item: usize::MAX,
            row: row.min(usize::MAX as u64) as usize,
            cell_count: cols,
            selected,
            shape_key: None,
        });
    }
    view
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

/// The selected column span of one block row, row-local and end-exclusive.
/// The selection covers inclusive cells `[a, b]` in (item, row, col) order.
fn selected_span(
    selection: Option<(FrozenPoint, FrozenPoint)>,
    item: usize,
    row: usize,
    cols: u32,
) -> Option<(u16, u16)> {
    let (a, b) = selection?;
    let here = (item, row);

    if here < (a.item, a.line) || here > (b.item, b.line) {
        return None;
    }

    let lo = if here == (a.item, a.line) { a.col } else { 0 };

    let hi = if here == (b.item, b.line) {
        b.col.saturating_add(1)
    } else {
        cols.max(1)
    }
    .min(cols.max(1));

    (lo < hi).then(|| {
        (
            lo.min(u16::MAX as u32) as u16,
            hi.min(u16::MAX as u32) as u16,
        )
    })
}

fn expand_wide_span(line: &TerminalLine, (mut start, mut end): (u16, u16)) -> (u16, u16) {
    for cell in line.cells() {
        if cell.wide != Wide::Wide {
            continue;
        }

        let spacer = cell.col.saturating_add(1);

        if start == spacer {
            start = cell.col;
        }

        if end == spacer {
            end = spacer.saturating_add(1);
        }
    }
    (start, end)
}

/// One deferred piece of a frozen selection: an inclusive cell range of one
/// engine block, formatted by the caller through `BlockRef::format_range`
/// AFTER releasing the store lock because the PTY thread nests
/// engine → store, so the reverse nesting would deadlock).
#[derive(Debug)]
pub(crate) struct FrozenSelectionPiece {
    pub handle: BlockHandle,
    /// `(row, col)` start within the block; `None` = the block's start.
    pub start: Option<(usize, u32)>,
    /// Inclusive `(row, col)` end within the block; `None` = the block's end.
    pub end: Option<(usize, u32)>,
}

/// The per-block ranges of the frozen selection (inclusive endpoints), in
/// item order. Join the formatted pieces with `\n`.
pub(crate) fn frozen_selection_pieces(
    store: &BlockStore,
    a: FrozenPoint,
    b: FrozenPoint,
) -> Vec<FrozenSelectionPiece> {
    let (a, b) = if a <= b { (a, b) } else { (b, a) };

    let mut out = Vec::new();

    for (item_idx, item) in store.items().iter().enumerate() {
        if item_idx < a.item || item_idx > b.item {
            continue;
        }

        let Some(handle) = item.handle() else {
            continue;
        };

        out.push(FrozenSelectionPiece {
            handle,
            start: (item_idx == a.item).then_some((a.line, a.col)),
            end: (item_idx == b.item).then_some((b.line, b.col)),
        });
    }
    out
}

/// Shape the visible frozen rows. Block rows cache by `(block_id,
/// generation, row)`; live-history rows hash their text.
pub(crate) fn shape_frozen_rows(
    rows: &[FrozenRow],
    cell_w: f32,
    window: &mut Window,
) -> Vec<ShapedLine> {
    terminal::terminal_view::shape_lines(
        rows.iter().map(|row| {
            (
                row.shape_key.unwrap_or_else(|| row.line.text_hash()),
                &row.line,
            )
        }),
        cell_w,
        window,
    )
}

/// Paint separators + frozen rows (backgrounds then glyphs).
pub(crate) fn paint_frozen(
    bounds: Bounds<Pixels>,
    view: &FrozenView,
    shaped: &[ShapedLine],
    cell_w: f32,
    cell_h: f32,
    window: &mut Window,
    cx: &mut App,
) {
    for row in &view.rows {
        terminal::terminal_view::paint_line_backgrounds_at(
            bounds, &row.line, row.y, cell_w, cell_h, window,
        );
    }

    // Selection tint under the glyphs (over the cell backgrounds).
    let selection_bg = theme_selection_background();

    for row in &view.rows {
        let Some((start, end)) = row.selected else {
            continue;
        };

        window.paint_quad(fill(
            Bounds::new(
                point(
                    bounds.left() + px(start as f32 * cell_w),
                    bounds.top() + px(row.y),
                ),
                size(px((end - start) as f32 * cell_w), px(cell_h)),
            ),
            rgb(selection_bg.rgb_u32()),
        ));
    }

    terminal::terminal_view::paint_glyph_rows(
        bounds,
        view.rows
            .iter()
            .zip(shaped)
            .map(|(row, line)| (row.y, line)),
        cell_h,
        window,
        cx,
    );
}

pub(crate) fn paint_frozen_separators(
    bounds: Bounds<Pixels>,
    separators: &[f32],
    window: &mut Window,
) {
    for y in separators {
        window.paint_quad(fill(
            terminal::terminal_view::block_separator_bounds(bounds, bounds.top() + px(*y), 1.0),
            Rgba {
                r: ((SEPARATOR_COLOR >> 16) & 0xff) as f32 / 255.0,
                g: ((SEPARATOR_COLOR >> 8) & 0xff) as f32 / 255.0,
                b: (SEPARATOR_COLOR & 0xff) as f32 / 255.0,
                a: 0.67,
            },
        ));
    }
}

pub(crate) fn paint_frozen_chrome(
    bounds: Bounds<Pixels>,
    items_chrome: &[FrozenItemChrome],
    window: &mut Window,
    cx: &mut App,
) {
    for chrome in items_chrome {
        let top = bounds.top() + px(chrome.top);
        let height = px(chrome.bottom - chrome.top);
        let gutter_alpha = if chrome.selected { 0xe6 } else { 0x59 };

        window.paint_quad(fill(
            Bounds::new(
                point(
                    bounds.left() - px(BLOCK_GUTTER_GAP + BLOCK_GUTTER_WIDTH),
                    top,
                ),
                size(px(BLOCK_GUTTER_WIDTH), height),
            ),
            rgba((chrome.accent << 8) | gutter_alpha),
        ));

        if chrome.selected {
            window.paint_quad(fill(
                Bounds::new(point(bounds.left(), top), size(bounds.size.width, height)),
                rgba(BLOCK_SELECTED_TINT),
            ));
        }
    }

    let style = window.text_style();
    let font_size = style.font_size.to_pixels(window.rem_size());
    for chrome in items_chrome {
        let Some(header) = chrome.header.as_deref() else {
            continue;
        };

        let runs = [TextRun {
            len: header.len(),
            font: style.font(),
            color: Hsla::from(rgb(0x7f8c98)),
            background_color: None,
            underline: None,
            strikethrough: None,
        }];

        let shaped = window.text_system().shape_line(
            SharedString::from(header.to_string()),
            font_size,
            &runs,
            Some(bounds.size.width),
        );

        let _ = shaped.paint(
            point(bounds.left(), bounds.top() + px(chrome.header_y)),
            px(0.0),
            TextAlign::Right,
            Some(bounds.size.width),
            window,
            cx,
        );
    }
}

#[cfg(test)]
mod tests;

/// Blank rows around each block for the current presentation: chrome shows
/// one pad row above and below; compact (Command Blocks off) packs block rows
/// contiguously like a classic grid. Every block-list geometry consumer must
/// use this one value per frame so heights, hit-testing, and scroll math agree.
pub(crate) fn block_pad_rows(cx: &App) -> f32 {
    if cx.global::<AppSettings>().command_blocks {
        terminal::block_list::ITEM_PAD_ROWS
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

#[derive(Clone, Copy, PartialEq)]
pub(crate) struct BlockListMeasureKey {
    /// (cols, cell height, pad rows) — pad rows toggling (Command Blocks
    /// on/off) changes every item height, so it must force a full remeasure.
    pub(crate) layout: (u32, f32, f32),
    pub(crate) store_len: usize,
    pub(crate) evicted_items: u64,
    pub(crate) last_item_px: f32,
    pub(crate) tail_px: f32,
    pub(crate) live_rows: usize,
}

pub(crate) struct BlockListRenderMetrics {
    pub(crate) store_len: usize,
    pub(crate) evicted_items: u64,
    pub(crate) item_count: usize,
    pub(crate) frozen_px: f32,
    /// The live item's history rows in pixels (active-grid scrollback above
    /// the live grid) — the "tail" position in scroll/active-top math.
    pub(crate) tail_px: f32,
    pub(crate) total_px: f32,
    pub(crate) offset_px: f32,
    pub(crate) last_item_px: f32,
}

pub(crate) fn block_list_render_metrics(
    store: &BlockStore,
    live_rows: usize,
    history_rows: u64,
    cols: u32,
    cell_h: f32,
    pad_rows: f32,
    offset: ListOffset,
) -> BlockListRenderMetrics {
    let items = store.items();
    let store_len = items.len();
    let item_count = store_len + 1;
    let mut frozen_px = 0.0;
    let mut offset_px = 0.0;
    let mut last_item_px = 0.0;

    for (ix, item) in items.iter().enumerate() {
        let item_px = terminal::block_list::item_px(item, cols, cell_h, pad_rows);
        if ix < offset.item_ix {
            offset_px += item_px;
        }
        if ix + 1 == store_len {
            last_item_px = item_px;
        }
        frozen_px += item_px;
    }

    let tail_px = history_rows as f32 * cell_h;
    let total_px =
        frozen_px + terminal::block_list::live_item_px(history_rows, live_rows, cell_h, pad_rows);
    if offset.item_ix >= item_count {
        offset_px = total_px;
    } else if offset.item_ix <= store_len {
        offset_px += offset.offset_in_item.as_f32();
    }

    BlockListRenderMetrics {
        store_len,
        evicted_items: store.evicted_items,
        item_count,
        frozen_px,
        tail_px,
        total_px,
        offset_px,
        last_item_px,
    }
}

pub(crate) fn block_list_live_chrome(
    live_index: usize,
    live_rows: usize,
    cell_h: f32,
    in_flight: Option<&InFlightBlock>,
    has_open_prompt: bool,
    selected: bool,
) -> Option<terminal::block_list::FrozenItemChrome> {
    let running = in_flight.is_some();
    if !running && !has_open_prompt {
        return None;
    }
    terminal::block_list::live_chrome(live_index, live_rows, cell_h, running, selected)
}

pub(crate) fn offset_frozen_chrome(
    mut chrome: terminal::block_list::FrozenItemChrome,
    item_top: f32,
) -> terminal::block_list::FrozenItemChrome {
    chrome.top += item_top;
    chrome.bottom += item_top;
    chrome.header_y += item_top;
    chrome
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

pub(crate) fn shift_selected_item_for_eviction(
    selected: Option<usize>,
    evicted_delta: usize,
    store_len: usize,
) -> Option<usize> {
    let selected = selected?;
    if selected < evicted_delta {
        None
    } else {
        let shifted = selected - evicted_delta;
        (shifted <= store_len).then_some(shifted)
    }
}

/// How to bring the mirrored GPUI `ListState` in line with the store after
/// front evictions and tail growth. Pure so the index arithmetic is testable
/// away from `ListState`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ListReconcile {
    /// Replace the mirror wholesale with the new item count.
    Reset,
    /// Drop `front_evict` items from the front, then replace the
    /// `tail_splice` range with that many new items.
    Patch {
        front_evict: usize,
        tail_splice: Option<(ops::Range<usize>, usize)>,
    },
}

pub(crate) fn plan_list_reconcile(
    mirrored_count: usize,
    evicted_delta: usize,
    item_count: usize,
) -> ListReconcile {
    let mut mirrored = mirrored_count;
    let mut front_evict = 0;

    if evicted_delta > 0 {
        // Only frozen items (all but the live tail) can be evicted from the
        // mirror; a delta beyond that means the mirror is too stale to patch.
        let old_frozen = mirrored.saturating_sub(1);

        if evicted_delta > old_frozen {
            return ListReconcile::Reset;
        }

        front_evict = evicted_delta;
        mirrored -= evicted_delta;
    }

    // A shrink beyond eviction (e.g. history cleared) invalidates the mirror.
    if item_count < mirrored {
        return ListReconcile::Reset;
    }

    let tail_splice = (item_count != mirrored).then(|| {
        // Replace the old live tail; the new items are the freshly frozen
        // blocks plus the new live tail.
        let old_live = mirrored.saturating_sub(1);
        (old_live..mirrored, item_count - old_live)
    });

    ListReconcile::Patch {
        front_evict,
        tail_splice,
    }
}

/// What the mirrored list must remeasure after this frame's metrics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RemeasureScope {
    /// Layout inputs (cols/cell/pad) changed: every item height is stale.
    All,
    /// Content changed: only the last frozen item and the live tail moved.
    Tail,
    None,
}

pub(crate) fn plan_remeasure(
    prev: Option<BlockListMeasureKey>,
    next: BlockListMeasureKey,
) -> RemeasureScope {
    if prev.is_some_and(|prev| prev.layout != next.layout) {
        RemeasureScope::All
    } else if prev != Some(next) {
        RemeasureScope::Tail
    } else {
        RemeasureScope::None
    }
}

#[cfg(test)]
mod layout_tests;
