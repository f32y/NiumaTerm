use nmt_terminal::clipboard::{Clipboard, ClipboardType};

use crate::pane_model::PaneController;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TerminalKeyAction {
    Write(Vec<u8>),
    /// Copy the selection, or write `bytes` when there is nothing selected.
    /// A chord that carries no byte for the shell leaves `bytes` empty and so
    /// does nothing when there is nothing to copy.
    CopyOrWrite(Vec<u8>),
    Paste,
    Ignore,
}

/// Distinguishes clipboard copies from other handled keys so UI feedback does
/// not fire when Ctrl-C writes ETX to the terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KeyOutcome {
    Ignored,
    Written,
    FrozenCopied,
    Copied,
}

impl PaneController {
    pub(crate) fn copy_text_to_clipboard(&self, text: String) -> bool {
        if text.is_empty() {
            return false;
        }

        let mut clipboard = Clipboard::default();

        clipboard.set(ClipboardType::Clipboard, text);
        true
    }

    pub(crate) fn apply_key_action(&mut self, action: TerminalKeyAction) -> KeyOutcome {
        match action {
            TerminalKeyAction::Write(bytes) => {
                if self.source.session.write_input(&bytes) {
                    KeyOutcome::Written
                } else {
                    KeyOutcome::Ignored
                }
            }
            TerminalKeyAction::CopyOrWrite(bytes) => {
                if let Some((a, b)) = self.frozen_drag.current() {
                    let text = self.source.session.frozen_selection_text(a, b);
                    if self.copy_text_to_clipboard(text) {
                        self.frozen_drag.clear();
                        return KeyOutcome::FrozenCopied;
                    }
                }
                if self.copy_selection() {
                    return KeyOutcome::Copied;
                }

                if self.source.session.write_input(&bytes) {
                    KeyOutcome::Written
                } else {
                    KeyOutcome::Ignored
                }
            }
            TerminalKeyAction::Paste => {
                if self.paste() {
                    KeyOutcome::Written
                } else {
                    KeyOutcome::Ignored
                }
            }
            TerminalKeyAction::Ignore => KeyOutcome::Ignored,
        }
    }

    fn paste(&self) -> bool {
        let mut clipboard = Clipboard::default();

        let text = clipboard.get(ClipboardType::Clipboard);

        self.source.session.paste_text(&text)
    }

    fn copy_selection(&self) -> bool {
        let Some(text) = self
            .source
            .session
            .selected_text()
            .filter(|text| !text.is_empty())
        else {
            return false;
        };

        let mut clipboard = Clipboard::default();

        clipboard.set(ClipboardType::Clipboard, text);
        self.source.session.clear_selection();

        true
    }
}
