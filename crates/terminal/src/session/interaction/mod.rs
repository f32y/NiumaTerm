//! Terminal input and selection rules in cell coordinates. Hosts own pointer
//! geometry, clipboard access, and reactions to completed operations.

use nmt_config::system::NewlineShortcut;

use crate::input::{TerminalKey, TerminalKeyAction, key_action};
use crate::render_buffer::RenderBuffer;
use crate::session::TerminalSession;
use crate::session::interaction::selection::{FrozenSelection, PendingExpansion};

mod copy;
mod selection;

pub use crate::session::interaction::copy::{CopyCompletion, PendingCopy};
pub use crate::session::interaction::selection::{
    block_selection_span, selection_type_for_click_count,
};

#[derive(Default)]
pub struct TerminalInteraction {
    frozen: FrozenSelection,
    pending_expansion: Option<PendingExpansion>,
    selection_generation: u64,
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
        &self,
        session: &TerminalSession,
        snapshot: &RenderBuffer,
        key: &TerminalKey<'_>,
        newline_shortcut: NewlineShortcut,
    ) -> InputOutcome {
        match key_action(key, newline_shortcut) {
            TerminalKeyAction::CopyOrWrite(bytes) => {
                if let Some(copy) = self.copy_selection(session, snapshot) {
                    return InputOutcome::CopyPending(copy);
                }
                Self::write(session, &bytes)
            }
            TerminalKeyAction::Write(bytes) => Self::write(session, &bytes),
            TerminalKeyAction::Paste => InputOutcome::PasteRequested,
            TerminalKeyAction::Ignore => InputOutcome::Ignored,
        }
    }

    fn write(session: &TerminalSession, bytes: &[u8]) -> InputOutcome {
        if session.write_input(bytes) {
            InputOutcome::Written
        } else {
            InputOutcome::Ignored
        }
    }
}

#[cfg(test)]
mod tests;
