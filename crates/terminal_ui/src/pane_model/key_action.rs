use std::path::PathBuf;

use nmt_terminal::clipboard::{Clipboard, ClipboardType};
use nmt_terminal::input::{TerminalKey, should_defer_to_ime};
use nmt_terminal::session::interaction::{CopyCompletion, InputOutcome, PendingCopy};

use crate::pane_model::PaneController;
use crate::pane_model::scroll::ScrollOutcome;

#[derive(Debug)]
pub(crate) enum KeyOutcome {
    Ignored,
    Written,
    CopyPending(PendingCopy),
    Scrolled(ScrollOutcome),
}

pub(crate) enum TextInput<'a> {
    Commit(&'a str),
    DropPaths(&'a [PathBuf]),
    RerunSelectedBlock,
}

impl PaneController {
    pub(crate) fn key_down(&mut self, key: &TerminalKey<'_>) -> KeyOutcome {
        if !self.source.session.alt_screen()
            && key.modifiers.is_empty()
            && !key.function
            && key.key.eq_ignore_ascii_case("end")
        {
            match self.scroll_to_latest() {
                ScrollOutcome::Ignored => {}
                outcome => return KeyOutcome::Scrolled(outcome),
            }
        }
        if should_defer_to_ime(key) {
            return KeyOutcome::Ignored;
        }
        self.send_key(key)
    }

    pub(crate) fn send_key(&mut self, key: &TerminalKey<'_>) -> KeyOutcome {
        match self.interaction.send_key(
            &self.source.session,
            &self.source.snapshot,
            key,
            self.settings.newline_shortcut,
        ) {
            InputOutcome::Ignored => KeyOutcome::Ignored,
            InputOutcome::Written => KeyOutcome::Written,
            InputOutcome::CopyPending(copy) => KeyOutcome::CopyPending(copy),
            InputOutcome::PasteRequested => {
                let text = Clipboard::default().get(ClipboardType::Clipboard);
                if self.source.session.paste_text(&text) {
                    KeyOutcome::Written
                } else {
                    KeyOutcome::Ignored
                }
            }
        }
    }

    pub(crate) fn write_text_input(&mut self, input: TextInput<'_>) -> bool {
        match input {
            TextInput::Commit(text) => self.source.session.write_text(text),
            TextInput::DropPaths(paths) => self.source.session.paste_paths(paths),
            TextInput::RerunSelectedBlock => self
                .gutter
                .selected()
                .is_some_and(|item| self.source.session.rerun_block(item)),
        }
    }

    pub(crate) fn copy_text_to_clipboard(&self, text: String) -> bool {
        !text.is_empty() && Clipboard::default().set(ClipboardType::Clipboard, text)
    }

    pub(crate) fn finish_copy(&mut self, text: String, completion: CopyCompletion) -> bool {
        if !self.copy_text_to_clipboard(text) {
            return false;
        }
        self.interaction
            .complete_copy(&self.source.session, &self.source.snapshot, completion);
        true
    }
}
