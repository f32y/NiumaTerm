use crate::terminal::block_list::*;

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
