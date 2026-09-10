use std::fs;
use std::path::{Path, PathBuf};

use crate::chat::MessageImage;
use crate::session::ImageAttachment;

/// Build inline images without writing files, so every supplied attachment
/// travels with the message even when a scratch directory is unavailable.
pub(super) fn inline_images<'a>(
    attachments: impl Iterator<Item = ImageAttachment<'a>>,
) -> Vec<MessageImage> {
    attachments
        .map(|attachment| MessageImage {
            bytes: attachment.bytes.to_vec(),
            media_type: attachment.media_type.to_string(),
        })
        .collect()
}

/// Write each attachment into `scratch`, returning the paths that could be
/// written. A file that cannot be written is left out rather than failing the
/// message: the text and the images that did land are still worth sending.
pub(super) fn write_attachments<'a>(
    attachments: impl Iterator<Item = ImageAttachment<'a>>,
    scratch: &Path,
) -> Vec<PathBuf> {
    let mut attachments = attachments.peekable();

    if attachments.peek().is_none() || fs::create_dir_all(scratch).is_err() {
        return Vec::new();
    }

    attachments
        .enumerate()
        .filter_map(|(index, attachment)| {
            // Position-based names overwrite matching images on later sends
            // instead of creating a new set of filenames for every turn.
            let path = scratch.join(format!("image-{}.png", index + 1));

            fs::write(&path, attachment.bytes).ok().map(|()| path)
        })
        .collect()
}

#[cfg(test)]
mod tests;
