use copypasta::ClipboardContext;
use raw_window_handle::RawDisplayHandle;
use tracing::warn;

use crate::clipboard::Clipboard;

impl Clipboard {
    /// # Safety
    /// The display handle must refer to a valid display connection.
    pub unsafe fn new(_display: RawDisplayHandle) -> Self {
        Self::default()
    }
}

impl Default for Clipboard {
    fn default() -> Self {
        match ClipboardContext::new() {
            Ok(clipboard) => Self {
                clipboard: Box::new(clipboard),
                selection: None,
            },
            Err(err) => {
                // Clipboard access can be unavailable while the terminal remains usable.
                warn!("Unable to initialize clipboard, falling back to no-op: {err}");

                Self::new_nop()
            }
        }
    }
}
