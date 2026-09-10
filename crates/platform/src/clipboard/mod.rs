// clipboard.rs was retired originally from https://github.com/alacritty/alacritty/blob/e35e5ad14fce8456afdd89f2b392b9924bb27471/alacritty/src/clipboard.rs
// which is licensed under Apache 2.0 license.

use copypasta::ClipboardProvider;
use copypasta::nop_clipboard::NopClipboardContext;
use tracing::warn;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardType {
    Clipboard,
    Selection,
}

pub struct Clipboard {
    pub(crate) clipboard: Box<dyn ClipboardProvider>,
    pub(crate) selection: Option<Box<dyn ClipboardProvider>>,
}

impl Clipboard {
    /// Used for tests and to handle missing clipboard provider when built without the `x11`
    /// feature.
    pub fn new_nop() -> Self {
        Self {
            clipboard: Box::new(NopClipboardContext::new().unwrap()),
            selection: None,
        }
    }
    pub fn set(&mut self, ty: ClipboardType, text: impl Into<String>) -> bool {
        let clipboard = match (ty, &mut self.selection) {
            (ClipboardType::Selection, Some(provider)) => provider,
            (ClipboardType::Selection, None) => return false,
            _ => &mut self.clipboard,
        };

        match clipboard.set_contents(text.into()) {
            Ok(()) => true,
            Err(err) => {
                warn!("Unable to store text in clipboard: {err}");
                false
            }
        }
    }

    pub fn get(&mut self, ty: ClipboardType) -> String {
        let clipboard = match (ty, &mut self.selection) {
            (ClipboardType::Selection, Some(provider)) => provider,
            _ => &mut self.clipboard,
        };

        match clipboard.get_contents() {
            Err(err) => {
                warn!("Unable to load text from clipboard: {}", err);
                String::new()
            }
            Ok(text) => text,
        }
    }
}

#[cfg(test)]
mod tests;
