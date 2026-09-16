//! Terminal input and selection rules in cell coordinates. Hosts own pointer
//! geometry, clipboard access, and reactions to completed operations.

#[cfg(test)]
#[path = "interaction_tests.rs"]
mod interaction_tests;

use std::collections::HashSet;

use futures::channel::oneshot;
use nmt_config::system::NewlineShortcut;

use crate::event::{BlockRange, Request};
use crate::ghostty::BlockHandle;
use crate::input::{KeyPhase, TerminalKey, TerminalKeyAction, key_action};
use crate::render_buffer::RenderBuffer;
use crate::selection::{SelectionRange, SelectionType};
use crate::session::{BlockPoint, TerminalSession};
use crate::vt_modes::Mode;

#[derive(Default)]
pub struct TerminalInteraction {
    frozen: FrozenSelection,
    pending_expansion: Option<PendingExpansion>,
    selection_generation: u64,
    pressed_keys: HashSet<String>,
}

#[derive(Debug)]
pub enum InputOutcome {
    Ignored,
    Written,
    CopyPending(PendingCopy),
    PasteRequested,
}

impl TerminalInteraction {
    pub fn send_key(
        &mut self,
        session: &TerminalSession,
        snapshot: &RenderBuffer,
        key: &TerminalKey<'_>,
        newline_shortcut: NewlineShortcut,
    ) -> InputOutcome {
        let mode = session.modes();

        if !mode.contains(Mode::REPORT_EVENT_TYPES) {
            self.pressed_keys.clear();
        }

        if key.phase == KeyPhase::Release && !self.pressed_keys.remove(key.key) {
            return InputOutcome::Ignored;
        }

        let outcome = match key_action(key, newline_shortcut, mode) {
            TerminalKeyAction::CopyOrWrite(bytes) => {
                if let Some(copy) = self.copy_selection(session, snapshot) {
                    return InputOutcome::CopyPending(copy);
                }

                Self::write(session, &bytes)
            }
            TerminalKeyAction::Write(bytes) => Self::write(session, &bytes),
            TerminalKeyAction::Paste => InputOutcome::PasteRequested,
            TerminalKeyAction::Ignore => InputOutcome::Ignored,
        };

        if matches!(outcome, InputOutcome::Written)
            && key.phase != KeyPhase::Release
            && mode.contains(Mode::REPORT_EVENT_TYPES)
        {
            self.pressed_keys.insert(key.key.to_owned());
        }

        outcome
    }

    pub fn clear_pressed_keys(&mut self) {
        self.pressed_keys.clear();
    }

    fn write(session: &TerminalSession, bytes: &[u8]) -> InputOutcome {
        if session.write_input(bytes) {
            InputOutcome::Written
        } else {
            InputOutcome::Ignored
        }
    }

    pub fn copy_selection(
        &self,
        session: &TerminalSession,
        snapshot: &RenderBuffer,
    ) -> Option<PendingCopy> {
        let (request, selection) = if let Some(pending) = &self.pending_expansion {
            (pending.copy_text(session), CopiedSelection::FrozenPending)
        } else if let Some((a, b)) = self.frozen.current() {
            (
                session.frozen_selection_text(a, b),
                CopiedSelection::Frozen(a, b),
            )
        } else {
            let range = session.selection_range_in(snapshot)?;

            (
                session.selected_text_in(snapshot)?,
                CopiedSelection::Live(range),
            )
        };

        Some(PendingCopy {
            request,
            completion: CopyCompletion {
                selection,
                generation: self.selection_generation,
            },
        })
    }

    /// Clear the copied selection only after the host accepts the text. A
    /// later pointer gesture owns its selection even if the old read finishes.
    pub fn complete_copy(
        &mut self,
        session: &TerminalSession,
        snapshot: &RenderBuffer,
        completion: CopyCompletion,
    ) {
        if completion.generation != self.selection_generation {
            return;
        }

        match completion.selection {
            CopiedSelection::FrozenPending => {
                self.pending_expansion = None;

                self.frozen.clear();
            }
            CopiedSelection::Frozen(a, b) if self.frozen.current() == Some((a, b)) => {
                self.frozen.clear();
            }
            CopiedSelection::Live(range) if session.selection_range_in(snapshot) == Some(range) => {
                session.clear_selection();
            }
            _ => {}
        }
    }

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

// ---------------------------------------------------------------------------
// Copy requests
// ---------------------------------------------------------------------------

#[derive(Debug)]
enum CopiedSelection {
    Live(SelectionRange),
    Frozen(BlockPoint, BlockPoint),
    FrozenPending,
    None,
}

/// Retains the selection that supplied a copy without letting a host mutate it.
#[derive(Debug)]
pub struct CopyCompletion {
    selection: CopiedSelection,
    generation: u64,
}

#[derive(Debug)]
pub struct PendingCopy {
    pub request: Request<String>,
    pub completion: CopyCompletion,
}

impl PendingCopy {
    pub fn ready(text: String) -> Self {
        let (reply, request) = oneshot::channel();
        let _ = reply.send(Ok(text));

        request.into()
    }
}

impl From<Request<String>> for PendingCopy {
    fn from(request: Request<String>) -> Self {
        Self {
            request,
            completion: CopyCompletion {
                selection: CopiedSelection::None,
                generation: 0,
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Frozen-region selection state
// ---------------------------------------------------------------------------

pub fn selection_type_for_click_count(click_count: usize) -> SelectionType {
    match click_count {
        2 => SelectionType::Semantic,
        3.. => SelectionType::Lines,
        _ => SelectionType::Simple,
    }
}

#[derive(Default)]
struct FrozenSelection {
    /// Frozen-region selection: (anchor, head), both inclusive cell points.
    selection: Option<(BlockPoint, BlockPoint)>,

    /// Anchor of an in-progress frozen-region drag. The selection itself is
    /// only created on the first mouse-move, so a plain click selects nothing
    /// (matching the engine's empty-selection-dropped-on-up semantics).
    anchor: Option<BlockPoint>,
}

impl FrozenSelection {
    /// Start a drag from `anchor`, dropping whatever was selected.
    fn begin(&mut self, anchor: BlockPoint) {
        self.selection = None;
        self.anchor = Some(anchor);
    }

    /// Take a range whole, as a double- or triple-click does. There is nothing
    /// left to drag from, so the anchor goes with it.
    fn select(&mut self, selection: Option<(BlockPoint, BlockPoint)>) {
        self.selection = selection;
        self.anchor = None;
    }

    fn anchor(&self) -> Option<BlockPoint> {
        self.anchor
    }

    /// Grow the selection to `head`, reporting whether a drag was in progress.
    fn extend(&mut self, head: BlockPoint) -> bool {
        let Some(anchor) = self.anchor else {
            return false;
        };

        self.selection = Some((anchor, head));

        true
    }

    /// End a drag without committing it, reporting whether one was open.
    fn commit(&mut self) -> bool {
        self.anchor.take().is_some()
    }

    /// Drop the selection, reporting whether one was showing.
    fn clear(&mut self) -> bool {
        self.selection.take().is_some()
    }

    fn current(&self) -> Option<(BlockPoint, BlockPoint)> {
        self.selection
    }
}

struct PendingExpansion {
    point: BlockPoint,
    handle: BlockHandle,
    request: Request<BlockRange>,
    kind: SelectionType,
}

impl PendingExpansion {
    fn copy_text(&self, session: &TerminalSession) -> Request<String> {
        session.block_selection_text(self.handle, self.point.line, self.point.col, self.kind)
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
