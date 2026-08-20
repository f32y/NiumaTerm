//! Images attached to the message being composed.
//!
//! The composer is a plain text field, so an attachment is tied to the message
//! by a literal `[Image #N]` placeholder in the text. That makes the text the
//! record of where each image belongs, and the only thing that can say whether
//! an attachment is still wanted: an attachment whose placeholder the user
//! deleted is gone, whichever way they deleted it.
//!
//! Reconciliation therefore runs one direction only, from the text. Removing a
//! thumbnail deletes its placeholder and reconciles, so both routes share one
//! rule rather than two that can disagree.

use std::env;
use std::io::Cursor;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;

use gpui::{Image, ImageFormat};
use image_rs::GenericImageView;
use image_rs::imageops::FilterType;

/// Images one message may carry. Claude Code's harness takes them inline on a
/// single stdin line, so a message that gathers many large screenshots is one
/// the harness would struggle to read.
pub(in crate::agent) const MAX_ATTACHMENTS: usize = 8;

/// Long-edge cap applied when an image is attached. A screenshot from a 4K
/// display is several megabytes encoded, and nothing in a conversation reads it
/// at full size.
pub(in crate::agent) const MAX_IMAGE_EDGE: u32 = 2048;

const PLACEHOLDER_PREFIX: &str = "[Image #";
const PLACEHOLDER_SUFFIX: char = ']';

/// One image attached to the pending message.
pub(in crate::agent) struct Attachment {
    /// Held in the form the renderer takes, shared rather than copied: the
    /// strip asks for it every frame, and the encoded bytes of a screenshot
    /// are megabytes.
    image: Arc<Image>,
    /// The text this attachment is anchored by, kept alongside it so removal
    /// and renumbering do not have to reconstruct it.
    placeholder: String,
}

impl Attachment {
    pub(in crate::agent) fn bytes(&self) -> &[u8] {
        self.image.bytes()
    }

    pub(in crate::agent) fn format(&self) -> ImageFormat {
        self.image.format()
    }

    pub(in crate::agent) fn placeholder(&self) -> &str {
        &self.placeholder
    }

    /// The renderable form of this attachment. Encoded bytes render directly,
    /// so a thumbnail costs no file on disk.
    pub(in crate::agent) fn image(&self) -> Arc<Image> {
        self.image.clone()
    }
}

/// Why a paste produced no attachment.
pub(in crate::agent) enum AttachError {
    /// The message already carries [`MAX_ATTACHMENTS`].
    Full,
    /// The clipboard's bytes could not be read as an image.
    Undecodable,
}

#[derive(Default)]
pub(in crate::agent) struct PendingAttachments {
    items: Vec<Attachment>,
}

impl PendingAttachments {
    pub(in crate::agent) fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub(in crate::agent) fn iter(&self) -> impl Iterator<Item = &Attachment> {
        self.items.iter()
    }

    pub(in crate::agent) fn clear(&mut self) {
        self.items.clear();
    }

    /// Decode a clipboard image, shrink it past the edge cap, and attach it.
    /// Returns the placeholder to insert at the caret.
    pub(in crate::agent) fn attach(&mut self, image: &Image) -> Result<String, AttachError> {
        if self.items.len() >= MAX_ATTACHMENTS {
            return Err(AttachError::Full);
        }

        let decoded =
            image_rs::load_from_memory(image.bytes()).map_err(|_| AttachError::Undecodable)?;
        let (width, height) = decoded.dimensions();

        // Re-encoding as PNG regardless of the clipboard's format keeps one
        // format flowing to the thumbnail, the transcript, and both harnesses.
        let bytes = match scaled_dimensions(width, height) {
            Some((to_width, to_height)) => {
                encode_png(&decoded.resize(to_width, to_height, FilterType::Triangle))?
            }
            None => encode_png(&decoded)?,
        };

        let placeholder = placeholder_text(self.items.len() + 1);
        self.items.push(Attachment {
            image: Arc::new(Image::from_bytes(ImageFormat::Png, bytes)),
            placeholder: placeholder.clone(),
        });

        Ok(placeholder)
    }

    /// The placeholder of the attachment at `index`, for a caller about to
    /// delete it from the composer text.
    pub(in crate::agent) fn placeholder_at(&self, index: usize) -> Option<&str> {
        self.items.get(index).map(|item| item.placeholder.as_str())
    }

    /// Drop attachments the text no longer names, order the survivors the way
    /// the text reads, and renumber them consecutively from 1. Returns the
    /// rewritten text when renumbering changed it.
    ///
    /// A placeholder naming no attachment is left alone: the user typed it, and
    /// it is theirs to send as text.
    pub(in crate::agent) fn reconcile(&mut self, text: &str) -> Option<String> {
        let spans = placeholder_spans(text);

        // Text order decides the new numbering, because that is the order a
        // reader meets the images in. Cutting a placeholder and pasting it
        // elsewhere therefore reorders the strip to match.
        let mut ordered: Vec<Attachment> = Vec::with_capacity(self.items.len());
        for (_, number) in &spans {
            let placeholder = placeholder_text(*number);
            if ordered.iter().any(|item| item.placeholder == placeholder) {
                continue;
            }
            if let Some(position) = self
                .items
                .iter()
                .position(|item| item.placeholder == placeholder)
            {
                ordered.push(self.items.remove(position));
            }
        }

        self.items = ordered;

        let renumbered = renumber(text, &spans, &self.items);
        for (index, item) in self.items.iter_mut().enumerate() {
            item.placeholder = placeholder_text(index + 1);
        }

        renumbered
    }
}

/// Rewrite every placeholder that names an attachment to that attachment's new
/// position. Written in one pass over the original spans so a reordering never
/// renames one placeholder onto another that still exists.
fn renumber(text: &str, spans: &[(Range<usize>, usize)], items: &[Attachment]) -> Option<String> {
    let mut rewritten = String::with_capacity(text.len());
    let mut cursor = 0;
    let mut changed = false;

    for (span, number) in spans {
        let old = placeholder_text(*number);
        let Some(position) = items.iter().position(|item| item.placeholder == old) else {
            continue;
        };
        let new = placeholder_text(position + 1);
        if new == old {
            continue;
        }

        rewritten.push_str(&text[cursor..span.start]);
        rewritten.push_str(&new);
        cursor = span.end;
        changed = true;
    }

    changed.then(|| {
        rewritten.push_str(&text[cursor..]);
        rewritten
    })
}

pub(in crate::agent) fn placeholder_text(number: usize) -> String {
    format!("{PLACEHOLDER_PREFIX}{number}{PLACEHOLDER_SUFFIX}")
}

/// Every `[Image #N]` in `text`, with the byte range it occupies and the number
/// it names, in the order they appear.
fn placeholder_spans(text: &str) -> Vec<(Range<usize>, usize)> {
    let mut spans = Vec::new();
    let mut cursor = 0;

    while let Some(offset) = text[cursor..].find(PLACEHOLDER_PREFIX) {
        let start = cursor + offset;
        let digits_at = start + PLACEHOLDER_PREFIX.len();
        let Some(length) = text[digits_at..].find(PLACEHOLDER_SUFFIX) else {
            break;
        };
        let digits = &text[digits_at..digits_at + length];
        cursor = digits_at + length + PLACEHOLDER_SUFFIX.len_utf8();

        if let Ok(number) = digits.parse::<usize>() {
            spans.push((start..cursor, number));
        }
    }

    spans
}

/// The size an image is shrunk to, or `None` when it already fits. The long
/// edge lands on the cap and the short edge is scaled by the same factor, so
/// the shape is kept.
fn scaled_dimensions(width: u32, height: u32) -> Option<(u32, u32)> {
    let long_edge = width.max(height);
    if long_edge <= MAX_IMAGE_EDGE {
        return None;
    }

    let scale = f64::from(MAX_IMAGE_EDGE) / f64::from(long_edge);
    let scaled = |edge: u32| ((f64::from(edge) * scale).round() as u32).max(1);

    Some((scaled(width), scaled(height)))
}

fn encode_png(image: &image_rs::DynamicImage) -> Result<Vec<u8>, AttachError> {
    let mut bytes = Vec::new();

    image
        .write_to(&mut Cursor::new(&mut bytes), image_rs::ImageFormat::Png)
        .map_err(|_| AttachError::Undecodable)?;

    Ok(bytes)
}

/// Where a pane writes the attachment files a harness reads by path. Keyed by
/// the pane's route so two panes cannot collide, and removed with the pane, so
/// nothing outlives the tab that pasted it.
pub(in crate::agent) fn scratch_dir(route: &str) -> PathBuf {
    let key: String = route
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();

    env::temp_dir().join(format!("niumaterm-agent-{key}"))
}

#[cfg(test)]
mod tests;
