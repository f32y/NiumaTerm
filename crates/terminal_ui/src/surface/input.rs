use nmt_terminal::clipboard::{Clipboard, ClipboardType};

use crate::surface::TerminalSurface;

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
pub(crate) enum TerminalKeyResult {
    Ignored,
    Handled,
    Copied,
}

impl TerminalSurface {
    pub(crate) fn apply_key_action(&self, action: TerminalKeyAction) -> TerminalKeyResult {
        match action {
            TerminalKeyAction::Write(bytes) => {
                if self.session.write_input(&bytes) {
                    TerminalKeyResult::Handled
                } else {
                    TerminalKeyResult::Ignored
                }
            }
            TerminalKeyAction::CopyOrWrite(bytes) => {
                if self.copy_selection() {
                    return TerminalKeyResult::Copied;
                }

                if self.session.write_input(&bytes) {
                    TerminalKeyResult::Handled
                } else {
                    TerminalKeyResult::Ignored
                }
            }
            TerminalKeyAction::Paste => {
                if self.paste() {
                    TerminalKeyResult::Handled
                } else {
                    TerminalKeyResult::Ignored
                }
            }
            TerminalKeyAction::Ignore => TerminalKeyResult::Ignored,
        }
    }

    fn paste(&self) -> bool {
        let mut clipboard = Clipboard::default();

        let text = clipboard.get(ClipboardType::Clipboard);

        self.session.paste_text(&text)
    }

    fn copy_selection(&self) -> bool {
        let Some(text) = self.session.selected_text().filter(|text| !text.is_empty()) else {
            return false;
        };

        let mut clipboard = Clipboard::default();

        clipboard.set(ClipboardType::Clipboard, text);
        self.session.clear_selection();

        true
    }
}
