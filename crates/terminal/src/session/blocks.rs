use crate::block_store::BlockStore;
use crate::ghostty::BlockHandle;

/// A position in the frozen history: store item, physical block row, column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct BlockPoint {
    pub item: usize,
    pub line: usize,
    pub col: u32,
}

/// One immutable request range, resolved by the engine owner.
#[derive(Debug)]
pub(super) struct FrozenSelectionPiece {
    pub handle: BlockHandle,

    /// `(row, col)` start within the block; `None` = the block's start.
    pub start: Option<(usize, u32)>,

    /// Inclusive `(row, col)` end within the block; `None` = the block's end.
    pub end: Option<(usize, u32)>,
}

/// The per-block ranges of the frozen selection (inclusive endpoints), in
/// item order. Join the formatted pieces with `\n`.
pub(super) fn frozen_selection_pieces(
    store: &BlockStore,
    a: BlockPoint,
    b: BlockPoint,
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
