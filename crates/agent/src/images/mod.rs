use std::io::Cursor;
#[cfg(test)]
mod tests;
use std::ops::Range;

use image_rs::GenericImageView as _;
use image_rs::imageops::FilterType;
/// Images one message may carry. Claude Code's harness takes them inline on a
/// single stdin line, so a message that gathers many large screenshots is one
/// the harness would struggle to read.
pub const MAX_ATTACHMENTS: usize = 8;

/// Long-edge cap applied when an image is attached. A screenshot from a 4K
/// display is several megabytes encoded, and nothing in a conversation reads it
/// at full size.
pub const MAX_IMAGE_EDGE: u32 = 2048;

const PLACEHOLDER_PREFIX: &str = "[Image #";
const PLACEHOLDER_SUFFIX: char = ']';

/// One image attached to the pending message.
pub struct Attachment<T> {
    /// Held in the form the renderer takes, shared rather than copied: the
    /// strip asks for it every frame, and the encoded bytes of a screenshot
    /// are megabytes.
    pub image: T,
    /// The text this attachment is anchored by, kept alongside it so removal
    /// and renumbering do not have to reconstruct it.
    placeholder: String,
}

impl<T> Attachment<T> {
    pub fn placeholder(&self) -> &str {
        &self.placeholder
    }
}

/// Why a paste produced no attachment.
pub enum AttachError {
    /// The message already carries [`MAX_ATTACHMENTS`].
    Full,
    /// The clipboard's bytes could not be read as an image.
    Undecodable,
}

/// The payload is caller-owned so a GUI can retain its native image handle.
/// Encoding transfers one buffer into that handle; reordering moves handles
/// without duplicating image bytes or requiring a second image cache.
pub struct PendingAttachments<T> {
    items: Vec<Attachment<T>>,
}

impl<T> Default for PendingAttachments<T> {
    fn default() -> Self {
        Self { items: Vec::new() }
    }
}

impl<T> PendingAttachments<T> {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Attachment<T>> {
        self.items.iter()
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }

    /// Decode a clipboard image, shrink it past the edge cap, and attach it.
    /// Returns the placeholder to insert at the caret.
    pub fn attach(
        &mut self,
        image: &[u8],
        make_image: impl FnOnce(Vec<u8>) -> T,
    ) -> Result<String, AttachError> {
        if self.items.len() >= MAX_ATTACHMENTS {
            return Err(AttachError::Full);
        }

        let decoded = image_rs::load_from_memory(image).map_err(|_| AttachError::Undecodable)?;
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
            image: make_image(bytes),
            placeholder: placeholder.clone(),
        });

        Ok(placeholder)
    }

    /// The image a `[Image #N]` placeholder names, counting from one the way
    /// the text reads. `None` where the number names nothing this message
    /// carries.
    fn image_for_number(&self, number: usize) -> Option<&T> {
        self.items
            .get(number.checked_sub(1)?)
            .map(|item| &item.image)
    }

    /// The byte range of every placeholder in `text` that names an image this
    /// message still carries, which is what the composer draws as a link. A
    /// placeholder naming no attachment stays plain text: the user typed it,
    /// and it stands for nothing to open.
    pub fn placeholder_links(&self, text: &str) -> Vec<Range<usize>> {
        placeholder_spans(text)
            .into_iter()
            .filter(|(_, number)| self.image_for_number(*number).is_some())
            .map(|(span, _)| span)
            .collect()
    }

    /// The image the placeholder at `range` names. The range is matched
    /// against a fresh reading of the text rather than trusted as an offset,
    /// so a range taken before an edit resolves to nothing rather than to
    /// whatever now sits at those bytes.
    pub fn linked_image(&self, text: &str, range: Range<usize>) -> Option<&T> {
        placeholder_spans(text)
            .into_iter()
            .find(|(span, _)| *span == range)
            .and_then(|(_, number)| self.image_for_number(number))
    }

    /// The placeholder of the attachment at `index`, for a caller about to
    /// delete it from the composer text.
    pub fn placeholder_at(&self, index: usize) -> Option<&str> {
        self.items.get(index).map(|item| item.placeholder.as_str())
    }

    /// Drop attachments the text no longer names, order the survivors the way
    /// the text reads, and renumber them consecutively from 1. Returns the
    /// rewritten text when renumbering changed it.
    ///
    /// A placeholder naming no attachment is left alone: the user typed it, and
    /// it is theirs to send as text.
    pub fn reconcile(&mut self, text: &str) -> Option<String> {
        let spans = placeholder_spans(text);

        // Text order decides the new numbering, because that is the order a
        // reader meets the images in. Cutting a placeholder and pasting it
        // elsewhere therefore reorders the strip to match.
        let mut ordered: Vec<Attachment<T>> = Vec::with_capacity(self.items.len());

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
fn renumber<T>(
    text: &str,
    spans: &[(Range<usize>, usize)],
    items: &[Attachment<T>],
) -> Option<String> {
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

pub fn placeholder_text(number: usize) -> String {
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
