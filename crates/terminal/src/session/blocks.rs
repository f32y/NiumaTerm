use crate::block_store::{BlockItem, BlockStore};
use crate::ghostty::{AcquiredBlock, BlockHandle};
use crate::selection::SelectionType;
use crate::session::TerminalSession;

/// A position in the frozen history: store item, physical block row, column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct BlockPoint {
    pub item: usize,
    pub line: usize,
    pub col: u32,
}

impl TerminalSession {
    /// Copy item metadata before acquiring the engine. The PTY worker takes
    /// the engine before the store, so these locks must never overlap here.
    pub fn block_snapshot(&self, item: usize) -> Option<(BlockItem, Option<AcquiredBlock>)> {
        let item = self.shared.block_store.lock().items().get(item)?.clone();
        let acquired = item.handle().and_then(|handle| self.acquire_block(handle));
        Some((item, acquired))
    }

    pub fn block_command(&self, item: usize) -> Option<String> {
        let store = self.shared.block_store.lock();
        if let Some(item) = store.items().get(item) {
            return item.meta.command.clone();
        }
        let live = item == store.items().len();
        drop(store);
        live.then(|| self.in_flight_block())
            .flatten()
            .map(|block| block.command)
    }

    pub fn block_text(&self, item: usize) -> Option<String> {
        let handle = self.block_handle(item)?;
        self.format_block_range(handle, None, None)
    }

    pub fn expand_frozen_selection(
        &self,
        at: BlockPoint,
        kind: SelectionType,
    ) -> Option<(BlockPoint, BlockPoint)> {
        let handle = self.block_handle(at.item)?;
        let ((start_line, start_col), (end_line, end_col)) =
            self.frozen_selection_range(handle, at.line, at.col, kind)?;
        Some((
            BlockPoint {
                item: at.item,
                line: start_line,
                col: start_col,
            },
            BlockPoint {
                item: at.item,
                line: end_line,
                col: end_col,
            },
        ))
    }

    pub fn frozen_selection_text(&self, a: BlockPoint, b: BlockPoint) -> String {
        let pieces = frozen_selection_pieces(&self.shared.block_store.lock(), a, b);
        pieces
            .into_iter()
            .filter_map(|piece| self.format_block_range(piece.handle, piece.start, piece.end))
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub(super) fn block_handle(&self, item: usize) -> Option<BlockHandle> {
        self.shared.block_store.lock().items().get(item)?.handle()
    }

    fn format_block_range(
        &self,
        handle: BlockHandle,
        start: Option<(usize, u32)>,
        end: Option<(usize, u32)>,
    ) -> Option<String> {
        self.acquire_block(handle)?
            .block
            .format_range_clamped(start, end, true, true)
    }
}

/// One deferred piece of a frozen selection: an inclusive cell range of one
/// engine block, formatted by the caller through `BlockRef::format_range`
/// AFTER releasing the store lock because the PTY thread nests
/// engine → store, so the reverse nesting would deadlock).
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
