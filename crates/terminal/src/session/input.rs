use std::path::PathBuf;

use nmt_input::bracket_paste;

use crate::session::TerminalSession;
use crate::terminal::Mode;

impl TerminalSession {
    pub fn paste_paths(&self, paths: &[PathBuf]) -> bool {
        let text = paths
            .iter()
            .map(|path| {
                let path = path.to_string_lossy();
                if path.contains(' ') {
                    format!("\"{path}\"")
                } else {
                    path.into_owned()
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        self.paste_text(&text)
    }

    pub fn rerun_block(&self, item: usize) -> bool {
        self.block_command(item)
            .is_some_and(|command| self.write_text(&format!("{command}\r")))
    }

    pub fn write_text(&self, text: &str) -> bool {
        self.write_input(text.as_bytes())
    }

    pub fn paste_text(&self, text: &str) -> bool {
        let Some(bytes) = paste_payload(text, self.modes().contains(Mode::BRACKETED_PASTE)) else {
            return false;
        };

        self.write_input(&bytes)
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
