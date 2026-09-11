use futures::channel::oneshot;
use nmt_terminal::clipboard::{Clipboard, ClipboardType};
use nmt_terminal::selection::SelectionRange;
use nmt_terminal::session::BlockPoint;
use nmt_terminal::session::request::Request;

use crate::pane_model::PaneController;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TerminalKeyAction {
    Write(Vec<u8>),
    CopyOrWrite(Vec<u8>),
    Paste,
    Ignore,
}

#[derive(Debug)]
pub(crate) enum KeyOutcome {
    Ignored,
    Written,
    CopyPending(PendingCopy),
}

#[derive(Debug)]
pub(crate) enum CopiedSelection {
    Live(SelectionRange),
    Frozen(BlockPoint, BlockPoint),
    FrozenPending,
    None,
}

#[derive(Debug)]
pub(crate) struct PendingCopy {
    pub request: Request<String>,
    pub selection: CopiedSelection,
    pub generation: u64,
}

impl PendingCopy {
    pub(crate) fn ready(text: String) -> Self {
        let (reply, request) = oneshot::channel();
        let _ = reply.send(Ok(text));
        Self {
            request,
            selection: CopiedSelection::None,
            generation: 0,
        }
    }
}

impl PaneController {
    pub(crate) fn copy_text_to_clipboard(&self, text: String) -> bool {
        !text.is_empty() && Clipboard::default().set(ClipboardType::Clipboard, text)
    }

    pub(crate) fn finish_copy(
        &mut self,
        text: String,
        selection: CopiedSelection,
        generation: u64,
    ) -> bool {
        if !self.copy_text_to_clipboard(text) {
            return false;
        }
        if generation != self.selection_generation {
            return true;
        }
        match selection {
            CopiedSelection::FrozenPending => {
                self.pending_expansion = None;
                self.frozen_drag.clear();
            }
            CopiedSelection::Frozen(a, b) if self.frozen_drag.current() == Some((a, b)) => {
                self.frozen_drag.clear();
            }
            CopiedSelection::Live(range)
                if self
                    .source
                    .session
                    .selection_range_in(&self.source.snapshot)
                    == Some(range) =>
            {
                self.source.session.clear_selection()
            }
            _ => {}
        }
        true
    }

    pub(crate) fn apply_key_action(&mut self, action: TerminalKeyAction) -> KeyOutcome {
        match action {
            TerminalKeyAction::CopyOrWrite(bytes) => {
                if let Some(pending) = &self.pending_expansion {
                    return KeyOutcome::CopyPending(PendingCopy {
                        request: pending.copy_text(self),
                        selection: CopiedSelection::FrozenPending,
                        generation: self.selection_generation,
                    });
                }
                if let Some((a, b)) = self.frozen_drag.current() {
                    return KeyOutcome::CopyPending(PendingCopy {
                        request: self.source.session.frozen_selection_text(a, b),
                        selection: CopiedSelection::Frozen(a, b),
                        generation: self.selection_generation,
                    });
                }
                if let Some(range) = self
                    .source
                    .session
                    .selection_range_in(&self.source.snapshot)
                    && let Some(request) =
                        self.source.session.selected_text_in(&self.source.snapshot)
                {
                    return KeyOutcome::CopyPending(PendingCopy {
                        request,
                        selection: CopiedSelection::Live(range),
                        generation: self.selection_generation,
                    });
                }
                if self.source.session.write_input(&bytes) {
                    KeyOutcome::Written
                } else {
                    KeyOutcome::Ignored
                }
            }
            TerminalKeyAction::Write(bytes) => {
                if self.source.session.write_input(&bytes) {
                    KeyOutcome::Written
                } else {
                    KeyOutcome::Ignored
                }
            }
            TerminalKeyAction::Paste => {
                let text = Clipboard::default().get(ClipboardType::Clipboard);
                if self.source.session.paste_text(&text) {
                    KeyOutcome::Written
                } else {
                    KeyOutcome::Ignored
                }
            }
            TerminalKeyAction::Ignore => KeyOutcome::Ignored,
        }
    }
}
