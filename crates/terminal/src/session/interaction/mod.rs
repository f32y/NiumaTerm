//! Terminal input and selection rules in cell coordinates. Hosts own pointer
//! geometry, clipboard access, and reactions to completed operations.

pub use crate::session::interaction::copy::{CopyCompletion, PendingCopy};
pub use crate::session::interaction::selection::{
    block_selection_span, selection_type_for_click_count,
};

mod copy;
mod selection;

#[cfg(test)]
mod tests;

use std::collections::HashSet;

use nmt_config::system::NewlineShortcut;

use crate::input::{KeyPhase, TerminalKey, TerminalKeyAction, key_action};
use crate::render_buffer::RenderBuffer;
use crate::selection::SelectionType;
use crate::session::interaction::copy::CopiedSelection;
use crate::session::interaction::selection::{FrozenSelection, PendingExpansion};
use crate::session::{BlockPoint, TerminalSession};
use crate::terminal::Mode;

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
