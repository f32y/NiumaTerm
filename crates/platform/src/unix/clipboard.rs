#[cfg(any(feature = "x11", target_os = "macos"))]
use copypasta::ClipboardContext;
#[cfg(all(feature = "x11", not(target_os = "macos")))]
use copypasta::x11_clipboard::{Primary as X11SelectionClipboard, X11ClipboardContext};
#[cfg(any(feature = "x11", target_os = "macos"))]
use tracing::warn;

use crate::clipboard::Clipboard;

/// The system clipboard, with the X11 primary selection where there is one.
pub(crate) fn system_clipboard() -> Clipboard {
    #[cfg(any(feature = "x11", target_os = "macos"))]
    return match ClipboardContext::new() {
        Ok(clipboard) => Clipboard {
            clipboard: Box::new(clipboard),
            #[cfg(target_os = "macos")]
            selection: None,
            #[cfg(not(target_os = "macos"))]
            selection: match X11ClipboardContext::<X11SelectionClipboard>::new() {
                Ok(selection) => Some(Box::new(selection)),
                Err(err) => {
                    warn!("Unable to initialize primary selection clipboard: {err}");

                    None
                }
            },
        },
        Err(err) => {
            // A missing display server must not prevent terminal startup.
            warn!("Unable to initialize clipboard, falling back to no-op: {err}");

            Clipboard::new_nop()
        }
    };

    #[cfg(not(any(feature = "x11", target_os = "macos")))]
    Clipboard::new_nop()
}
