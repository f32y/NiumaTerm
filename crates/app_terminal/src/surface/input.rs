#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TerminalKeyAction {
    Write(Vec<u8>),
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
    pub fn write_text(&self, text: &str) -> bool {
        self.write_bytes(text.as_bytes())
    }

    pub(super) fn write_bytes(&self, bytes: &[u8]) -> bool {
        if bytes.is_empty() || self.read_only.load(Ordering::Relaxed) {
            return false;
        }

        self.session.write_input(bytes);

        true
    }

    pub(crate) fn apply_key_action(&self, action: TerminalKeyAction) -> TerminalKeyResult {
        match action {
            TerminalKeyAction::Write(bytes) => {
                if self.write_bytes(&bytes) {
                    TerminalKeyResult::Handled
                } else {
                    TerminalKeyResult::Ignored
                }
            }
            TerminalKeyAction::CopyOrWrite(bytes) => {
                if self.copy_selection() {
                    return TerminalKeyResult::Copied;
                }

                if self.write_bytes(&bytes) {
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

        self.paste_text(&text)
    }

    pub(crate) fn paste_text(&self, text: &str) -> bool {
        let Some(bytes) = paste_payload(text, self.modes().contains(Mode::BRACKETED_PASTE)) else {
            return false;
        };

        self.write_bytes(&bytes)
    }
}

pub(super) fn paste_payload(text: &str, bracketed: bool) -> Option<Vec<u8>> {
    if text.is_empty() {
        return None;
    }

    let mut body = text.replace("\r\n", "\r").replace('\n', "\r");

    if bracketed {
        body = body.replace("\x1b[201~", "");
    }

    Some(bracket_paste(body.as_bytes(), bracketed))
}
use std::sync::atomic::Ordering;

use nmt_input::bracket_paste;
use nmt_terminal::clipboard::{Clipboard, ClipboardType};
use nmt_terminal::terminal::Mode;

use crate::surface::TerminalSurface;
