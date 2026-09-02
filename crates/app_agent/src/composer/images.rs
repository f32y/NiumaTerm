//! Images attached to the message being composed, from the composer's side.
//!
//! An attachment is anchored to the text by a placeholder the user can edit or
//! delete like any other word, so the store is reconciled against the text
//! rather than driven by the buttons on the row.

use std::fs;
use std::path::Path;

use gpui::{ClipboardEntry, Context, Image, ImageFormat, Window};
use nmt_i18n::i18n;

use crate::AgentPane;
use crate::composer::CommandFeedbackKind;
use crate::composer::attachments::{AttachError, MAX_ATTACHMENTS};

/// A copied file read as an image, or `None` for anything that is not one.
/// Only the extension is trusted to decide whether reading is worth it; the
/// decode decides whether it was an image.
fn image_file(path: &Path) -> Option<Image> {
    let format = match path
        .extension()
        .and_then(|extension| extension.to_str())?
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => ImageFormat::Png,
        "jpg" | "jpeg" => ImageFormat::Jpeg,
        "webp" => ImageFormat::Webp,
        "gif" => ImageFormat::Gif,
        "bmp" => ImageFormat::Bmp,
        _ => return None,
    };

    Some(Image::from_bytes(format, fs::read(path).ok()?))
}

impl AgentPane {
    /// Take a pasted image into the pending message, reporting whether the
    /// paste was consumed. A paste this leaves alone falls through to the
    /// composer's own text handling, which is what a clipboard holding text
    /// should get.
    pub(crate) fn paste_image(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        // An image reaches the clipboard two ways: as pixels, from a capture
        // tool or a browser, and as a file, from a file manager. Both are the
        // same gesture to the person doing it.
        let Some(image) = cx
            .read_from_clipboard()
            .into_iter()
            .flat_map(|item| item.into_entries())
            .find_map(|entry| match entry {
                ClipboardEntry::Image(image) => Some(image),
                ClipboardEntry::ExternalPaths(paths) => {
                    paths.paths().iter().find_map(|path| image_file(path))
                }
                ClipboardEntry::String(_) => None,
            })
        else {
            return false;
        };

        if !self.kind.caps().image_input {
            self.palette.set_feedback(
                CommandFeedbackKind::Error,
                i18n("agent-composer-images-unsupported").replace("{name}", self.kind.display()),
                cx,
            );
            return true;
        }

        match self
            .attachments
            .attach_image(&image, &self.input, window, cx)
        {
            Ok(()) => {
                cx.notify();
                true
            }
            Err(AttachError::Full) => {
                self.palette.set_feedback(
                    CommandFeedbackKind::Error,
                    i18n("agent-composer-images-full")
                        .replace("{count}", &MAX_ATTACHMENTS.to_string()),
                    cx,
                );
                true
            }
            // Something on the clipboard claimed to be an image and was not.
            // Falling through lets the composer paste whatever text is there.
            Err(AttachError::Undecodable) => false,
        }
    }

    pub(crate) fn remove_attachment(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .attachments
            .remove_image(index, &self.input, window, cx)
        {
            cx.notify();
        }
    }

    pub(crate) fn sync_attachments(
        &mut self,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.attachments.sync(text, &self.input, window, cx) {
            cx.notify();
        }
    }
}
