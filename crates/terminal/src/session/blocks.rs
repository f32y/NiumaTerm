use futures::channel::oneshot;

use crate::block_store::{BlockItem, BlockStore};
use crate::event::Msg;
use crate::ghostty::BlockHandle;
use crate::selection::SelectionType;
use crate::session::TerminalSession;
use crate::session::request::{BlockRange, Query, Request, TextPiece, TextSource};

/// A position in the frozen history: store item, physical block row, column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct BlockPoint {
    pub item: usize,
    pub line: usize,
    pub col: u32,
}

impl TerminalSession {
    pub fn block_selection_text(
        &self,
        handle: BlockHandle,
        line: usize,
        col: u32,
        kind: SelectionType,
    ) -> Request<String> {
        self.request_text(TextSource::BlockSelection {
            handle,
            line,
            col,
            kind,
        })
    }

    pub fn block_item(&self, item: usize) -> Option<BlockItem> {
        self.shared.block_store.lock().items().get(item).cloned()
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

    pub fn block_text(&self, item: usize) -> Option<Request<String>> {
        let handle = self.block_handle(item)?;
        Some(self.request_text(TextSource::Blocks(vec![TextPiece {
            handle,
            start: None,
            end: None,
        }])))
    }

    pub fn expand_frozen_selection(
        &self,
        at: BlockPoint,
        kind: SelectionType,
    ) -> Option<Request<BlockRange>> {
        let handle = self.block_handle(at.item)?;
        let (reply, request) = oneshot::channel();
        let _ = self.messenger.send(Msg::Query(Query::ExpandSelection {
            handle,
            line: at.line,
            col: at.col,
            kind,
            reply,
        }));
        Some(request)
    }

    pub fn frozen_selection_text(&self, a: BlockPoint, b: BlockPoint) -> Request<String> {
        let pieces = frozen_selection_pieces(&self.shared.block_store.lock(), a, b);
        self.request_text(TextSource::Blocks(
            pieces
                .into_iter()
                .map(|piece| TextPiece {
                    handle: piece.handle,
                    start: piece.start,
                    end: piece.end,
                })
                .collect(),
        ))
    }

    pub(super) fn request_text(&self, source: TextSource) -> Request<String> {
        let (reply, request) = oneshot::channel();
        let _ = self
            .messenger
            .send(Msg::Query(Query::Text { source, reply }));
        request
    }

    pub(super) fn block_handle(&self, item: usize) -> Option<BlockHandle> {
        self.shared.block_store.lock().items().get(item)?.handle()
    }
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
