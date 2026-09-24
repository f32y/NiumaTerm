use copypasta::ClipboardContext;
use tracing::warn;

use crate::clipboard::Clipboard;

/// The system clipboard. Windows has no primary selection.
pub(crate) fn system_clipboard() -> Clipboard {
    match ClipboardContext::new() {
        Ok(clipboard) => Clipboard {
            clipboard: Box::new(clipboard),
            selection: None,
        },
        Err(err) => {
            // Clipboard access can be unavailable while the terminal remains usable.
            warn!("Unable to initialize clipboard, falling back to no-op: {err}");

            Clipboard::new_nop()
        }
    }
}
