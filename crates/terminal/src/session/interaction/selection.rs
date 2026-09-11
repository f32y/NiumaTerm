use crate::ghostty::BlockHandle;
use crate::selection::SelectionType;
use crate::session::interaction::TerminalInteraction;
use crate::session::request::{BlockRange, Request};
use crate::session::{BlockPoint, TerminalSession};

pub fn selection_type_for_click_count(click_count: usize) -> SelectionType {
    match click_count {
        2 => SelectionType::Semantic,
        3.. => SelectionType::Lines,
        _ => SelectionType::Simple,
    }
}

#[derive(Default)]
pub(super) struct FrozenSelection {
    /// Frozen-region selection: (anchor, head), both inclusive cell points.
    selection: Option<(BlockPoint, BlockPoint)>,
    /// Anchor of an in-progress frozen-region drag. The selection itself is
    /// only created on the first mouse-move, so a plain click selects nothing
    /// (matching the engine's empty-selection-dropped-on-up semantics).
    anchor: Option<BlockPoint>,
}

impl FrozenSelection {
    /// Start a drag from `anchor`, dropping whatever was selected.
    pub(super) fn begin(&mut self, anchor: BlockPoint) {
        self.selection = None;
        self.anchor = Some(anchor);
    }

    /// Take a range whole, as a double- or triple-click does. There is nothing
    /// left to drag from, so the anchor goes with it.
    pub(super) fn select(&mut self, selection: Option<(BlockPoint, BlockPoint)>) {
        self.selection = selection;
        self.anchor = None;
    }

    pub(super) fn anchor(&self) -> Option<BlockPoint> {
        self.anchor
    }

    /// Grow the selection to `head`, reporting whether a drag was in progress.
    pub(super) fn extend(&mut self, head: BlockPoint) -> bool {
        let Some(anchor) = self.anchor else {
            return false;
        };

        self.selection = Some((anchor, head));

        true
    }

    /// End a drag without committing it, reporting whether one was open.
    pub(super) fn commit(&mut self) -> bool {
        self.anchor.take().is_some()
    }

    /// Drop the selection, reporting whether one was showing.
    pub(super) fn clear(&mut self) -> bool {
        self.selection.take().is_some()
    }

    pub(super) fn current(&self) -> Option<(BlockPoint, BlockPoint)> {
        self.selection
    }
}

pub(super) struct PendingExpansion {
    point: BlockPoint,
    handle: BlockHandle,
    request: Request<BlockRange>,
    kind: SelectionType,
}

impl PendingExpansion {
    pub(super) fn copy_text(&self, session: &TerminalSession) -> Request<String> {
        session.block_selection_text(self.handle, self.point.line, self.point.col, self.kind)
    }
}

impl TerminalInteraction {
    pub fn begin_pointer(&mut self) {
        self.selection_generation = self.selection_generation.wrapping_add(1);
        self.pending_expansion = None;
    }

    pub fn select_block(
        &mut self,
        session: &TerminalSession,
        point: BlockPoint,
        kind: SelectionType,
    ) {
        session.clear_selection();
        if kind == SelectionType::Simple {
            self.frozen.begin(point);
        } else {
            self.frozen.clear();
            if let Some(handle) = session
                .block_item(point.item)
                .and_then(|item| item.handle())
                && let Some(request) = session.expand_frozen_selection(point, kind)
            {
                self.pending_expansion = Some(PendingExpansion {
                    point,
                    handle,
                    request,
                    kind,
                });
            }
        }
    }

    pub fn block_anchor(&self) -> Option<BlockPoint> {
        self.frozen.anchor()
    }

    pub fn block_selection(&self) -> Option<(BlockPoint, BlockPoint)> {
        self.frozen.current()
    }

    pub fn extend_block_selection(&mut self, head: BlockPoint) -> bool {
        self.frozen.extend(head)
    }

    pub fn commit_block_selection(&mut self) -> bool {
        self.frozen.commit()
    }

    pub fn clear_block_selection(&mut self) -> bool {
        self.frozen.clear()
    }

    pub fn poll_expansion(&mut self, session: &TerminalSession) {
        let Some(mut pending) = self.pending_expansion.take() else {
            return;
        };
        match pending.request.try_recv() {
            Ok(Some(Ok(((start_line, start_col), (end_line, end_col))))) => {
                let current = session
                    .block_item(pending.point.item)
                    .and_then(|item| item.handle());
                if current.is_some_and(|handle| {
                    handle.id == pending.handle.id && handle.generation == pending.handle.generation
                }) {
                    self.frozen.select(Some((
                        BlockPoint {
                            item: pending.point.item,
                            line: start_line,
                            col: start_col,
                        },
                        BlockPoint {
                            item: pending.point.item,
                            line: end_line,
                            col: end_col,
                        },
                    )));
                }
            }
            Ok(None) => self.pending_expansion = Some(pending),
            _ => {}
        }
    }
}

/// The selected column span of one block row, row-local and end-exclusive.
/// The selection covers inclusive cells `[a, b]` in (item, row, col) order.
pub fn block_selection_span(
    selection: Option<(BlockPoint, BlockPoint)>,
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
