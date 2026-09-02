use crate::block_list::*;

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

/// What the block list painted last frame, and which item the gutter has
/// selected.
///
/// The hit-test rows, the item chrome and the separator positions are all
/// recorded by one prepaint and read by the pointer events and paints that
/// follow it, so they are cleared and rebuilt as one. The selected item is an
/// index into that same record, which is what the jump, copy and re-run
/// actions target.
#[derive(Default)]
pub(crate) struct FrozenGutterSelection {
    /// Hit-test data recorded from the last native list prepaint.
    hit: FrozenHitInfo,
    /// Visible frozen item chrome recorded from native list item bounds.
    chrome: Vec<FrozenItemChrome>,
    /// Visible separator y positions, painted outside GPUI List's content mask.
    separators: Vec<f32>,
    /// The gutter-selected frozen item: highlighted and targeted by the
    /// copy, re-run and jump actions in list mode.
    selected: Option<usize>,
}

impl FrozenGutterSelection {
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

    /// The item whose gutter row covers `y`, if any.
    pub(crate) fn item_at(&self, y: f32) -> Option<usize> {
        self.chrome
            .iter()
            .find(|chrome| (chrome.top..chrome.bottom).contains(&y))
            .map(|chrome| chrome.item)
    }

    pub(crate) fn selected(&self) -> Option<usize> {
        self.selected
    }

    pub(crate) fn select(&mut self, item: usize) {
        self.selected = Some(item);
    }

    /// Drop the gutter selection, reporting whether one was showing.
    pub(crate) fn clear_selection(&mut self) -> bool {
        self.selected.take().is_some()
    }

    /// Follow the selected item through a store eviction, so the highlight
    /// stays on the same command rather than on the same index.
    pub(crate) fn shift_for_eviction(&mut self, evicted: usize, store_len: usize) {
        self.selected = shift_selected_item_for_eviction(self.selected, evicted, store_len);
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

/// The selected column span of one block row, row-local and end-exclusive.
/// The selection covers inclusive cells `[a, b]` in (item, row, col) order.
pub(super) fn selected_span(
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

pub(super) fn expand_wide_span(
    line: &TerminalLine,
    (mut start, mut end): (u16, u16),
) -> (u16, u16) {
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
