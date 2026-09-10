#[cfg(any(feature = "x11", target_os = "macos"))]
use copypasta::ClipboardContext;
#[cfg(all(feature = "wayland", not(target_os = "macos")))]
use copypasta::wayland_clipboard;
#[cfg(all(feature = "x11", not(target_os = "macos")))]
use copypasta::x11_clipboard::{Primary as X11SelectionClipboard, X11ClipboardContext};
use raw_window_handle::RawDisplayHandle;
#[cfg(any(feature = "x11", target_os = "macos"))]
use tracing::warn;

use crate::clipboard::Clipboard;

impl Clipboard {
    /// # Safety
    /// For Wayland, the display connection must remain valid until this
    /// clipboard is dropped, including any background work by its provider.
    pub unsafe fn new(display: RawDisplayHandle) -> Self {
        match display {
            #[cfg(all(feature = "wayland", not(target_os = "macos")))]
            RawDisplayHandle::Wayland(display) => {
                let (selection, clipboard) = unsafe {
                    wayland_clipboard::create_clipboards_from_external(display.display.as_ptr())
                };

                Self {
                    clipboard: Box::new(clipboard),
                    selection: Some(Box::new(selection)),
                }
            }
            _ => Self::default(),
        }
    }
}

impl Default for Clipboard {
    fn default() -> Self {
        #[cfg(any(feature = "x11", target_os = "macos"))]
        return match ClipboardContext::new() {
            Ok(clipboard) => Self {
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
                Self::new_nop()
            }
        };

        #[cfg(not(any(feature = "x11", target_os = "macos")))]
        Self::new_nop()
    }
}
