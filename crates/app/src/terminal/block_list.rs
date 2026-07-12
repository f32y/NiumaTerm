//! Block-split list rendering.
//!
//! Renders history as a real vertical list: frozen items above — each a
//! finished engine block read directly through a refcounted `BlockRef` —
//! plus one live item for the current engine viewport (with the active
//! grid's scrollback rows rendered above it while a command runs).
//! Scrolling is pure UI state over the list — the engine viewport stays
//! pinned at the bottom.

use gpui::{
    Bounds, FollowMode, ListAlignment, ListState, Pixels, ShapedLine, SharedString, TextAlign,
    TextRun, Window, point, px, size,
};
use nmt_terminal::block_store::{BlockItem, BlockStore, SegmentMeta};
use nmt_terminal::terminal::square::Wide;

use crate::terminal::frame::{
    StyleRun, TerminalCell, TerminalColor, TerminalLine, line_from_parts, theme_default_foreground,
    theme_selection_background,
};

/// Blank rows above and below each item's content: one full cell row on each
/// side, with the separator rule on the item's top edge — so adjacent blocks
/// read as content / blank / rule / blank / content.
pub(crate) const ITEM_PAD_ROWS: f32 = 1.0;
/// 1px separator rule inside the gap.
const SEPARATOR_COLOR: u32 = 0x3b4252;

pub(crate) struct BlockListState {
    /// Bumped per in-flight tick so only the newest duration repaint fires.
    pub tick_gen: u64,
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
            tick_gen: 0,
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

/// Pane-side hit-test data for the last built frozen view (small copy; the
/// full view moves into the element).
#[derive(Default, Clone)]
pub(crate) struct FrozenHitInfo {
    /// (y, item, row, cell_count) per visible frozen row.
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
    ) -> Option<FrozenPoint> {
        let (_, item, row, cell_count) =
            *self
                .rows
                .iter()
                .take_while(|(ry, ..)| *ry <= y)
                .last()
                .filter(|(ry, ..)| y < ry + cell_h * (1.0 + ITEM_PAD_ROWS))?;
        if item == usize::MAX {
            return None; // live-history sentinel: not addressable
        }
        let local = (x / cell_w.max(1.0)).floor().max(0.0) as u32;
        let col = local.min(cols.saturating_sub(1)).min(cell_count);
        Some(FrozenPoint {
            item,
            line: row,
            col,
        })
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
    pub generation: std::sync::Arc<crate::terminal::graphics::ImageGeneration>,
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

/// Pixel height of one item: content rows plus a blank pad row above and
/// below. Empty items (empty commands never freeze, but a stale cache can
/// briefly report 0) are invisible — no rows, no pads.
pub(crate) fn item_px(item: &BlockItem, cols: u32, cell_h: f32) -> f32 {
    match item_rows(item, cols) {
        0 => 0.0,
        rows => (rows as f32 + 2.0 * ITEM_PAD_ROWS) * cell_h,
    }
}

/// Pixel height of the live item: pads + the active grid's scrolled-out
/// history rows + the live grid's content rows. Shared by the item element's
/// layout and the render metrics so the two cannot drift.
pub(crate) fn live_item_px(history_rows: u64, live_rows: usize, cell_h: f32) -> f32 {
    history_rows as f32 * cell_h + (live_rows as f32 + 2.0 * ITEM_PAD_ROWS) * cell_h
}

/// Gutter/header accent for a frozen item, keyed off the exit code.
fn item_accent(meta: &SegmentMeta) -> u32 {
    match meta.exit_code {
        None => crate::terminal::view::BLOCK_RUNNING_COLOR,
        Some(0) => crate::terminal::view::BLOCK_SUCCESS_COLOR,
        Some(_) => crate::terminal::view::BLOCK_FAILURE_COLOR,
    }
}

/// Header label of a frozen item: truncated command + status/duration.
/// `None` without a command (nothing meaningful to show).
fn item_header(meta: &SegmentMeta) -> Option<String> {
    let command = meta.command.as_deref()?;
    if meta.exit_code.is_none() {
        return running_header(command, meta.started_at);
    }
    let duration = meta
        .started_at
        .zip(meta.ended_at)
        .and_then(|(s, e)| e.duration_since(s).ok())
        .map(format_duration);
    let status = match (meta.exit_code, duration) {
        (Some(0), Some(d)) => format!("✓ {d}"),
        (Some(0), None) => "✓".to_string(),
        (Some(code), Some(d)) => format!("✗ {code} · {d}"),
        (Some(code), None) => format!("✗ {code}"),
        (None, _) => unreachable!("running items returned above"),
    };
    Some(command_header(command, &status))
}

fn running_header(command: &str, started_at: Option<std::time::SystemTime>) -> Option<String> {
    if command.trim().is_empty() {
        return None;
    }
    let status = started_at
        .and_then(|started_at| std::time::SystemTime::now().duration_since(started_at).ok())
        .map(format_duration)
        .map(|duration| format!("⟳ {duration}"))
        .unwrap_or_else(|| "⟳".to_string());
    Some(command_header(command, &status))
}

fn command_header(command: &str, status: &str) -> String {
    format!(
        "{} · {status}",
        crate::terminal::view::truncate_command(command, 32)
    )
}

/// Chrome of the live item: a running command (`Some((command, started_at))` —
/// running accent + elapsed header) or the idle input region (`None` — input
/// accent, no header). `rows == 0` → invisible.
pub(crate) fn live_chrome(
    item: usize,
    rows: usize,
    cell_h: f32,
    running: Option<(&str, std::time::SystemTime)>,
    selected: bool,
) -> Option<FrozenItemChrome> {
    if rows == 0 {
        return None;
    }
    let (accent, header) = match running {
        Some((command, started_at)) => (
            crate::terminal::view::BLOCK_RUNNING_COLOR,
            running_header(command, Some(started_at)),
        ),
        None => (crate::terminal::view::BLOCK_INPUT_COLOR, None),
    };
    Some(FrozenItemChrome {
        item,
        top: 0.0,
        bottom: rows as f32 * cell_h,
        header_y: 0.0,
        accent,
        header,
        selected,
    })
}

/// `1.2s` / `815ms` / `2m05s` — the header's duration label.
pub(crate) fn format_duration(d: std::time::Duration) -> String {
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
/// NBSP), wide glyphs get an NBSP placeholder column, runs merge on equal
/// style. Spacer cells are dropped. Mirrors the frame extractor's display
/// conventions so frozen rows shape and paint like live ones.
#[derive(Default)]
pub(crate) struct EngineRowBuilder {
    text: String,
    cells: Vec<TerminalCell>,
    runs: Vec<StyleRun>,
    col: u16,
}

impl EngineRowBuilder {
    pub(crate) fn push(
        &mut self,
        x: u16,
        cell_text: nmt_terminal::ghostty::CellText,
        wide: nmt_terminal::ghostty::CellWide,
        style: &nmt_terminal::ghostty::SnapshotStyle,
        default_fg: TerminalColor,
    ) {
        use nmt_terminal::ghostty::CellWide;
        match wide {
            CellWide::SpacerTail | CellWide::SpacerHead => return,
            CellWide::Narrow | CellWide::Wide => {}
        }
        while self.col < x {
            self.text.push('\u{00a0}');
            push_run(
                &mut self.runs,
                '\u{00a0}'.len_utf8(),
                default_fg,
                false,
                false,
                false,
                false,
            );
            self.col += 1;
        }

        let to_tc = |c: nmt_terminal::ghostty::Color| TerminalColor {
            r: c.r,
            g: c.g,
            b: c.b,
        };
        let (fg, bg) = if style.inverse {
            (
                style.bg.map(to_tc).unwrap_or(default_fg),
                Some(style.fg.map(to_tc).unwrap_or(default_fg)),
            )
        } else {
            (
                style.fg.map(to_tc).unwrap_or(default_fg),
                style.bg.map(to_tc),
            )
        };

        let is_wide = wide == CellWide::Wide;
        let display: String = if cell_text.is_empty() {
            "\u{00a0}".into()
        } else {
            cell_text.replace([' ', '\t'], "\u{00a0}")
        };
        let mut seg_len = display.len();
        self.text.push_str(&display);
        if is_wide {
            self.text.push('\u{00a0}');
            seg_len += '\u{00a0}'.len_utf8();
        }
        push_run(
            &mut self.runs,
            seg_len,
            fg,
            style.bold,
            style.italic,
            style.underline != nmt_terminal::ghostty::Underline::None,
            style.strikethrough,
        );

        self.cells.push(TerminalCell {
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
        line_from_parts(self.text, self.cells, self.runs)
    }
}

/// Append a style run, merging into the previous run on equal style.
fn push_run(
    runs: &mut Vec<StyleRun>,
    len: usize,
    fg: TerminalColor,
    bold: bool,
    italic: bool,
    underline: bool,
    strikethrough: bool,
) {
    match runs.last_mut() {
        Some(last)
            if last.fg == fg
                && last.bold == bold
                && last.italic == italic
                && last.underline == underline
                && last.strikethrough == strikethrough =>
        {
            last.len += len;
        }
        _ => runs.push(StyleRun {
            len,
            fg,
            bold,
            italic,
            underline,
            strikethrough,
        }),
    }
}

/// Chrome inputs of a block item, cloneable out of the store lock —
/// prepaint acquires the engine `BlockRef` afterwards, and the store and
/// engine locks must never nest (surface lock discipline).
#[derive(Clone)]
pub(crate) struct HandleItemInfo {
    pub handle: nmt_terminal::ghostty::BlockHandle,
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
) -> std::ops::Range<usize> {
    const OVERDRAW: f32 = 260.0;
    let pad = ITEM_PAD_ROWS * cell_h;
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
    block: Option<(
        &nmt_terminal::ghostty::BlockRef,
        &nmt_terminal::ghostty::Palette,
    )>,
    info: &HandleItemInfo,
    item_idx: usize,
    visible: std::ops::Range<usize>,
    cell_h: f32,
    selection: Option<(FrozenPoint, FrozenPoint)>,
    selected_item: Option<usize>,
) -> FrozenView {
    let default_fg = theme_default_foreground();
    let selection = selection.map(|(a, b)| if a <= b { (a, b) } else { (b, a) });
    let rows = info.rows;
    let pad = ITEM_PAD_ROWS * cell_h;
    let mut view = FrozenView {
        rows: Vec::new(),
        items_chrome: Vec::new(),
        separators: Vec::new(),
        images: Vec::new(),
        active_top: (rows as f32 + 2.0 * ITEM_PAD_ROWS) * cell_h,
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
        view.rows.push(FrozenRow {
            y: pad + row as f32 * cell_h,
            line: builder.finish(),
            item: item_idx,
            row,
            cell_count: cols,
            selected: selected_span(selection, item_idx, row, cols),
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
    placements: &[nmt_terminal::ghostty::PlacementScreenPos],
    generations: &std::collections::HashMap<
        u32,
        std::sync::Arc<crate::terminal::graphics::ImageGeneration>,
    >,
    visible: &std::ops::Range<usize>,
    cell_h: f32,
) -> Vec<FrozenImage> {
    let pad = ITEM_PAD_ROWS * cell_h;
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
fn block_row_shape_key(handle: nmt_terminal::ghostty::BlockHandle, row: usize) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (handle.id, handle.generation, row).hash(&mut hasher);
    hasher.finish()
}

/// The live item's scrolled-up history: active-grid scrollback rows read as
/// physical lines rendered above the live grid. Rows carry
/// the unselectable sentinel — the live region uses the engine selection.
pub(crate) fn live_history_view(
    lines: Vec<(u64, TerminalLine)>,
    total_rows: u64,
    cols: u32,
    cell_h: f32,
) -> FrozenView {
    let pad = ITEM_PAD_ROWS * cell_h;
    let mut view = FrozenView {
        rows: Vec::new(),
        items_chrome: Vec::new(),
        separators: vec![0.0],
        images: Vec::new(),
        active_top: pad + total_rows as f32 * cell_h,
    };
    for (row, line) in lines {
        view.rows.push(FrozenRow {
            y: pad + row as f32 * cell_h,
            line,
            item: usize::MAX,
            row: row.min(usize::MAX as u64) as usize,
            cell_count: cols,
            selected: None,
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
    from_px: f32,
    direction: i8,
) -> Option<f32> {
    let mut tops = Vec::new();
    let mut y = 0.0f32;
    for item in store.items() {
        if item_rows(item, cols) > 0 {
            tops.push(y);
        }
        y += item_px(item, cols, cell_h);
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

/// One deferred piece of a frozen selection: an inclusive cell range of one
/// engine block, formatted by the caller through `BlockRef::format_range`
/// AFTER releasing the store lock because the PTY thread nests
/// engine → store, so the reverse nesting would deadlock).
#[derive(Debug)]
pub(crate) struct FrozenSelectionPiece {
    pub handle: nmt_terminal::ghostty::BlockHandle,
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
    let style = window.text_style();
    let font_size = style.font_size.to_pixels(window.rem_size());
    let base = style.to_run(0);
    rows.iter()
        .map(|row| {
            let runs = crate::terminal::view::terminal_text_runs(&row.line, &base);
            window.text_system().shape_line_by_hash(
                row.shape_key.unwrap_or_else(|| row.line.text_hash()),
                row.line.text().len(),
                font_size,
                &runs,
                Some(px(cell_w)),
                || row.line.text().clone(),
            )
        })
        .collect()
}

/// Paint separators + frozen rows (backgrounds then glyphs).
pub(crate) fn paint_frozen(
    bounds: Bounds<Pixels>,
    view: &FrozenView,
    shaped: &[ShapedLine],
    cell_w: f32,
    cell_h: f32,
    window: &mut Window,
    cx: &mut gpui::App,
) {
    for row in &view.rows {
        crate::terminal::view::paint_line_backgrounds_at(
            bounds, &row.line, row.y, cell_w, cell_h, window,
        );
    }
    // Selection tint under the glyphs (over the cell backgrounds).
    let selection_bg = theme_selection_background();
    for row in &view.rows {
        let Some((start, end)) = row.selected else {
            continue;
        };
        window.paint_quad(gpui::fill(
            Bounds::new(
                point(
                    bounds.left() + px(start as f32 * cell_w),
                    bounds.top() + px(row.y),
                ),
                size(px((end - start) as f32 * cell_w), px(cell_h)),
            ),
            gpui::rgb(selection_bg.rgb_u32()),
        ));
    }
    for (row, line) in view.rows.iter().zip(shaped) {
        let _ = line.paint(
            point(bounds.left(), bounds.top() + px(row.y)),
            px(cell_h),
            gpui::TextAlign::Left,
            None,
            window,
            cx,
        );
    }
}

pub(crate) fn paint_frozen_separators(
    bounds: Bounds<Pixels>,
    separators: &[f32],
    window: &mut Window,
) {
    for y in separators {
        window.paint_quad(gpui::fill(
            crate::terminal::view::block_separator_bounds(bounds, bounds.top() + px(*y), 1.0),
            gpui::Rgba {
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
    cx: &mut gpui::App,
) {
    for chrome in items_chrome {
        let top = bounds.top() + px(chrome.top);
        let height = px(chrome.bottom - chrome.top);
        let gutter_alpha = if chrome.selected { 0xe6 } else { 0x59 };
        window.paint_quad(gpui::fill(
            Bounds::new(
                point(
                    bounds.left()
                        - px(crate::terminal::view::BLOCK_GUTTER_GAP
                            + crate::terminal::view::BLOCK_GUTTER_WIDTH),
                    top,
                ),
                size(px(crate::terminal::view::BLOCK_GUTTER_WIDTH), height),
            ),
            gpui::rgba((chrome.accent << 8) | gutter_alpha),
        ));
        if chrome.selected {
            window.paint_quad(gpui::fill(
                Bounds::new(point(bounds.left(), top), size(bounds.size.width, height)),
                gpui::rgba(crate::terminal::view::BLOCK_SELECTED_TINT),
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
            color: gpui::Hsla::from(gpui::rgb(0x7f8c98)),
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
mod tests {
    use nmt_terminal::event::BlockEvent;
    use nmt_terminal::ghostty::{BlockHandle, GhosttyTerminal};

    use super::*;

    fn row_texts(view: &FrozenView) -> Vec<String> {
        view.rows
            .iter()
            .map(|r| {
                r.line
                    .text()
                    .replace('\u{00a0}', " ")
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    fn finished_block(vt: &[u8], cols: u16, rows: u16) -> (GhosttyTerminal, HandleItemInfo) {
        let mut t = GhosttyTerminal::new(cols, rows, 10_000).unwrap();
        t.write_vt(vt);
        let handle = t.finish_block().unwrap().expect("block created");
        let rows = t.block_row_count(handle).unwrap();
        let info = HandleItemInfo {
            handle,
            rows,
            accent: crate::terminal::view::BLOCK_SUCCESS_COLOR,
            header: Some("cmd · ✓".into()),
        };
        (t, info)
    }

    /// A finished engine block renders as physical rows with chrome, item-
    /// local geometry matching `item_px`, and `(id, generation, row)` shape
    /// keys.
    #[test]
    fn frozen_block_view_reads_engine_rows() {
        let (t, info) = finished_block(b"hello\r\n\x1b[1mbold\r\n", 10, 4);
        assert_eq!(info.rows, 2);
        let (block, palette) = (
            t.block_acquire(info.handle).expect("acquire"),
            t.color_palette(),
        );

        let view = frozen_block_view(
            Some((&block, &palette)),
            &info,
            3,
            0..info.rows,
            10.0,
            None,
            Some(3),
        );
        assert_eq!(row_texts(&view), ["hello", "bold"]);
        assert_eq!(view.rows[0].y, 10.0, "content after the top pad row");
        assert_eq!(view.rows[1].y, 20.0);
        assert_eq!(view.active_top, 40.0, "rows + 2 pad rows");
        assert_eq!(view.separators, [0.0]);
        let chrome = &view.items_chrome[0];
        assert_eq!((chrome.item, chrome.top, chrome.bottom), (3, 0.0, 40.0));
        assert!(chrome.selected);
        assert!(view.rows[0].shape_key.is_some());
        assert_ne!(
            view.rows[0].shape_key, view.rows[1].shape_key,
            "per-row cache keys"
        );
        assert_eq!(view.rows[0].row, 0);

        // Styled reads carry through the visitor.
        assert!(view.rows[1].line.runs().iter().any(|r| r.bold));
    }

    /// Only the requested row range materializes; skipped head rows keep
    /// their item-local y so geometry never shifts.
    #[test]
    fn frozen_block_view_windows_visible_rows() {
        let (t, info) = finished_block(b"r0\r\nr1\r\nr2\r\n", 10, 5);
        assert_eq!(info.rows, 3);
        let (block, palette) = (
            t.block_acquire(info.handle).expect("acquire"),
            t.color_palette(),
        );
        let view = frozen_block_view(Some((&block, &palette)), &info, 0, 1..2, 10.0, None, None);
        assert_eq!(row_texts(&view), ["r1"]);
        assert_eq!(view.rows[0].y, 20.0, "pad + one skipped row");
        assert_eq!(view.active_top, 50.0, "full item height regardless");
    }

    /// A stale/reflowing block (`None`) still renders chrome at the cached
    /// height, so layout never jumps while content is briefly unavailable.
    #[test]
    fn frozen_block_view_placeholder_keeps_height() {
        let info = HandleItemInfo {
            handle: BlockHandle {
                id: 1,
                generation: 1,
            },
            rows: 4,
            accent: 0,
            header: None,
        };
        let view = frozen_block_view(None, &info, 0, 0..4, 10.0, None, None);
        assert!(view.rows.is_empty());
        assert_eq!(view.active_top, 60.0);
        assert_eq!(view.items_chrome.len(), 1);
    }

    /// Selection spans map straight onto physical rows.
    #[test]
    fn frozen_block_view_selection_spans_rows() {
        let (t, info) = finished_block(b"aaaa\r\nbbbb\r\ncccc\r\n", 10, 5);
        let (block, palette) = (
            t.block_acquire(info.handle).expect("acquire"),
            t.color_palette(),
        );
        let sel = Some((
            FrozenPoint {
                item: 0,
                line: 0,
                col: 2,
            },
            FrozenPoint {
                item: 0,
                line: 2,
                col: 1,
            },
        ));
        let view = frozen_block_view(
            Some((&block, &palette)),
            &info,
            0,
            0..info.rows,
            10.0,
            sel,
            None,
        );
        let spans: Vec<Option<(u16, u16)>> = view.rows.iter().map(|r| r.selected).collect();
        assert_eq!(
            spans,
            [Some((2, 10)), Some((0, 10)), Some((0, 2))],
            "endpoint rows partial, middle row full width"
        );
    }

    /// `frozen_selection_pieces` produces one per-block range with block-edge
    /// endpoints resolved per item.
    #[test]
    fn selection_pieces_cover_block_ranges() {
        let mut store = BlockStore::default();
        store.apply([
            BlockEvent::EngineBlock {
                seq: 1,
                handle: BlockHandle {
                    id: 6,
                    generation: 1,
                },
                rows: 2,
            },
            BlockEvent::EngineBlock {
                seq: 2,
                handle: BlockHandle {
                    id: 7,
                    generation: 1,
                },
                rows: 5,
            },
        ]);
        let pieces = frozen_selection_pieces(
            &store,
            FrozenPoint {
                item: 0,
                line: 0,
                col: 2,
            },
            FrozenPoint {
                item: 1,
                line: 3,
                col: 4,
            },
        );
        assert_eq!(pieces.len(), 2);
        assert_eq!(pieces[0].handle.id, 6);
        assert_eq!(pieces[0].start, Some((0, 2)));
        assert_eq!(pieces[0].end, None, "selection continues past this item");
        assert_eq!(pieces[1].handle.id, 7);
        assert_eq!(pieces[1].start, None, "selection starts before this item");
        assert_eq!(pieces[1].end, Some((3, 4)));
    }

    /// `item_rows`/`item_px` use the cached engine row count.
    #[test]
    fn item_geometry_uses_cached_rows() {
        let mut store = BlockStore::default();
        store.apply([BlockEvent::EngineBlock {
            seq: 1,
            handle: BlockHandle {
                id: 1,
                generation: 1,
            },
            rows: 7,
        }]);
        let item = &store.items()[0];
        assert_eq!(item_rows(item, 80), 7);
        assert_eq!(item_px(item, 80, 10.0), 90.0, "7 rows + 2 pad rows");
        assert_eq!(live_item_px(3, 2, 10.0), 70.0, "history + live + pads");
    }

    /// The visible-row window clamps to the item and pads with overdraw.
    #[test]
    fn visible_rows_clamps_to_item() {
        // Item fully above the viewport (scrolled past): empty range.
        assert_eq!(visible_rows(-10_000.0, 50, 600.0, 10.0), 50..50);
        // Item starting far below the viewport bottom: empty range.
        assert_eq!(visible_rows(10_000.0, 50, 600.0, 10.0), 0..0);
        // Item spanning the viewport: rows around the visible band only.
        let range = visible_rows(-1000.0, 1000, 600.0, 10.0);
        assert!(range.start > 0 && range.end < 1000);
        assert!(range.contains(&100), "row at viewport top included");
    }

    /// The hit map resolves pixel positions to (item, row, col) and refuses
    /// the live-history sentinel.
    #[test]
    fn hit_test_maps_pixels_to_frozen_points() {
        let mut hit = FrozenHitInfo::default();
        hit.push_row(10.0, 0, 0, 10); // item 0 row 0 at y=10
        hit.push_row(20.0, 0, 1, 10);
        hit.push_row(50.0, usize::MAX, 0, 10); // live-history sentinel
        hit.set_active_top(70.0);

        assert_eq!(
            hit.hit_test(35.0, 15.0, 10.0, 10.0, 10),
            Some(FrozenPoint {
                item: 0,
                line: 0,
                col: 3
            })
        );
        assert_eq!(
            hit.hit_test(15.0, 25.0, 10.0, 10.0, 10),
            Some(FrozenPoint {
                item: 0,
                line: 1,
                col: 1
            })
        );
        // Beyond the row width clamps to the last column.
        assert_eq!(
            hit.hit_test(500.0, 15.0, 10.0, 10.0, 10),
            Some(FrozenPoint {
                item: 0,
                line: 0,
                col: 9
            })
        );
        assert_eq!(hit.hit_test(0.0, 55.0, 10.0, 10.0, 10), None, "sentinel");
        assert_eq!(hit.hit_test(0.0, 5.0, 10.0, 10.0, 10), None, "above rows");
    }

    /// Chrome accents and headers key off the metadata.
    #[test]
    fn chrome_keys_off_metadata() {
        let mut store = BlockStore::default();
        store.apply([
            BlockEvent::EngineBlock {
                seq: 1,
                handle: BlockHandle {
                    id: 1,
                    generation: 1,
                },
                rows: 2,
            },
            BlockEvent::EngineBlock {
                seq: 2,
                handle: BlockHandle {
                    id: 2,
                    generation: 1,
                },
                rows: 1,
            },
        ]);
        let t0 = std::time::UNIX_EPOCH;
        store.update_meta(1, |m| {
            m.command = Some("build".into());
            m.exit_code = Some(0);
            m.started_at = Some(t0);
            m.ended_at = Some(t0 + std::time::Duration::from_secs(2));
        });
        store.update_meta(2, |m| {
            m.command = Some("bad".into());
            m.exit_code = Some(127);
        });

        let info1 = handle_item_info(&store.items()[0]).unwrap();
        assert_eq!(info1.accent, crate::terminal::view::BLOCK_SUCCESS_COLOR);
        assert_eq!(info1.header.as_deref(), Some("build · ✓ 2.0s"));
        let info2 = handle_item_info(&store.items()[1]).unwrap();
        assert_eq!(info2.accent, crate::terminal::view::BLOCK_FAILURE_COLOR);
        assert_eq!(info2.header.as_deref(), Some("bad · ✗ 127"));
    }

    /// Previous/next navigation walks item tops with edge no-ops.
    #[test]
    fn nav_item_top_walks_items() {
        let mut store = BlockStore::default();
        store.apply([
            BlockEvent::EngineBlock {
                seq: 1,
                handle: BlockHandle {
                    id: 1,
                    generation: 1,
                },
                rows: 1,
            },
            BlockEvent::EngineBlock {
                seq: 2,
                handle: BlockHandle {
                    id: 2,
                    generation: 1,
                },
                rows: 2,
            },
            BlockEvent::EngineBlock {
                seq: 3,
                handle: BlockHandle {
                    id: 3,
                    generation: 1,
                },
                rows: 1,
            },
        ]);
        // Heights: 30, 40, 30 → tops 0, 30, 70.
        assert_eq!(nav_item_top(&store, 80, 10.0, 0.0, 1), Some(30.0));
        assert_eq!(nav_item_top(&store, 80, 10.0, 30.0, 1), Some(70.0));
        assert_eq!(nav_item_top(&store, 80, 10.0, 70.0, 1), None);
        assert_eq!(nav_item_top(&store, 80, 10.0, 70.0, -1), Some(30.0));
        assert_eq!(nav_item_top(&store, 80, 10.0, 0.0, -1), None);
    }

    #[test]
    fn live_chrome_marks_running_pending_item() {
        let running = Some(("build", std::time::UNIX_EPOCH));
        let chrome = live_chrome(3, 2, 10.0, running, true).unwrap();
        assert_eq!((chrome.item, chrome.top, chrome.bottom), (3, 0.0, 20.0));
        assert_eq!(chrome.accent, crate::terminal::view::BLOCK_RUNNING_COLOR);
        assert!(chrome.header.as_deref().unwrap().starts_with("build · ⟳ "));
        assert!(chrome.selected);

        assert!(live_chrome(3, 0, 10.0, running, false).is_none());
    }

    #[test]
    fn live_chrome_marks_idle_prompt() {
        let chrome = live_chrome(2, 3, 10.0, None, true).unwrap();
        assert_eq!((chrome.item, chrome.top, chrome.bottom), (2, 0.0, 30.0));
        assert_eq!(chrome.accent, crate::terminal::view::BLOCK_INPUT_COLOR);
        assert_eq!(chrome.header, None);
        assert!(chrome.selected);

        assert!(live_chrome(2, 0, 10.0, None, false).is_none());
    }

    /// The live-history view positions sentinel rows and the active top.
    #[test]
    fn live_history_view_positions_rows() {
        let lines = vec![
            (0u64, line_from_parts("a".into(), Vec::new(), Vec::new())),
            (2u64, line_from_parts("c".into(), Vec::new(), Vec::new())),
        ];
        let view = live_history_view(lines, 3, 10, 10.0);
        assert_eq!(view.rows.len(), 2);
        assert_eq!(view.rows[0].y, 10.0, "pad + row 0");
        assert_eq!(view.rows[1].y, 30.0, "pad + row 2 (row 1 not visible)");
        assert_eq!(view.rows[0].item, usize::MAX, "unselectable sentinel");
        assert_eq!(view.active_top, 40.0, "pad + total history rows");
    }

    /// Frozen Kitty direct read: a placement frozen into a
    /// block reports a block-relative row, its pixels read back lazily, and
    /// the paint mapping lands on the right visible row band.
    #[test]
    fn frozen_block_images_map_visible_rows() {
        use crate::terminal::graphics::{ReleaseQueue, graphic_to_generation};

        let mut t = GhosttyTerminal::new(20, 5, 10_000).unwrap();
        t.resize(20, 5, 10, 20).unwrap(); // cell pixel size for grid math
        t.write_vt(b"a\r\nb\r\n");
        t.write_vt(b"\x1b_Ga=T,f=32,s=1,v=1,i=1;/wAA/w==\x1b\\");
        let handle = t.finish_block().unwrap().expect("block created");
        let block = t.block_acquire(handle).expect("acquire");

        let placements = t.block_placements(&block);
        assert_eq!(placements.len(), 1, "one frozen placement");
        let p = placements[0];
        assert_eq!(p.image_id, 1);
        assert_eq!((p.screen_col, p.screen_row), (0, 2), "block-relative row");
        assert!(p.grid_cols >= 1 && p.grid_rows >= 1);

        let data = t.block_image_pixels(&block, 1).expect("frozen pixels");
        assert_eq!((data.width, data.height), (1, 1));
        assert!(t.block_image_pixels(&block, 999).is_none(), "unknown id");

        let q: ReleaseQueue = Default::default();
        let generation = graphic_to_generation(data, &q).unwrap();
        let mut generations = std::collections::HashMap::new();
        generations.insert(1u32, generation);

        let images = frozen_block_images(&placements, &generations, &(0..3), 10.0);
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].y, 10.0 + 2.0 * 10.0, "pad + block row 2");
        assert_eq!((images[0].col, images[0].width), (0, p.grid_cols));

        // Rows outside the visible window materialize nothing.
        assert!(frozen_block_images(&placements, &generations, &(0..2), 10.0).is_empty());
        // A missing generation is skipped (retry next frame), not painted.
        assert!(frozen_block_images(&placements, &Default::default(), &(0..3), 10.0).is_empty());
    }

    /// Kitty V1 per-block ownership intentionally differs from active-screen ownership:
    /// cross-block place-by-id falls flat on the fresh
    /// screen, and an active delete-all cannot reach a frozen block's images.
    #[test]
    fn kitty_v1_per_block_ownership_deviations() {
        let mut t = GhosttyTerminal::new(20, 5, 10_000).unwrap();
        t.resize(20, 5, 10, 20).unwrap();
        t.write_vt(b"\x1b_Ga=T,f=32,s=1,v=1,i=7;/wAA/w==\x1b\\");
        let frozen = t.finish_block().unwrap().expect("block created");

        // Cross-block place-by-id: the new screen's storage is empty, so a
        // A placement-only command references nothing; a future implementation could
        // forward the image definition table if this pattern matters).
        t.write_vt(b"\x1b_Ga=p,i=7\x1b\\");
        assert!(!t.kitty_image_exists(7), "new screen storage starts empty");

        // Active delete-all only touches active storage so frozen blocks remain immutable:
        // frozen block keeps showing its freeze-time pixels.
        t.write_vt(b"\x1b_Ga=d\x1b\\");
        let block = t.block_acquire(frozen).expect("acquire");
        assert!(
            t.block_image_pixels(&block, 7).is_some(),
            "frozen pixels survive an active delete-all"
        );
        assert_eq!(t.block_placements(&block).len(), 1);
    }
}
