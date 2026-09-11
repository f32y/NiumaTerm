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
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;

use gpui::{Context, Entity, Image, ImageFormat, Window};
use gpui_component::input::TextareaState;
use nmt_agent::images::{
    Attachment as CoreAttachment, PendingAttachments as CorePendingAttachments,
};

use crate::AgentPane;

pub(super) mod render;

/// The placeholder as it is written into the composer. A space on each side
/// keeps it a word of its own, so the prompt around it does not run into the
/// marker. The leading one is dropped where there is nothing to separate it
/// from: the start of the text, or whitespace the caret already sits after.
pub(crate) fn spaced_placeholder(preceding: Option<char>, placeholder: &str) -> String {
    match preceding {
        Some(character) if !character.is_whitespace() => format!(" {placeholder} "),
        _ => format!("{placeholder} "),
    }
}

/// Everything the pending message carries besides its text: the images
/// anchored in it by placeholder, and the earlier response text quoted into
/// it. Both are cleared by the same send and drawn on the same strip above
/// the composer, so they travel together.
#[derive(Default)]
pub(crate) struct ComposerAttachments {
    images: PendingAttachments,

    /// Earlier agent response text attached to the pending message.
    annotations: Vec<String>,
}

impl ComposerAttachments {
    pub(crate) fn images(&self) -> &PendingAttachments {
        &self.images
    }

    pub(crate) fn annotations(&self) -> &[String] {
        &self.annotations
    }

    pub(crate) fn clear_images(&mut self) {
        self.images.clear();
    }

    pub(crate) fn clear_annotations(&mut self) {
        self.annotations.clear();
    }

    /// Put annotations back in front of the ones already pending, which is
    /// what an interrupted message restores.
    pub(crate) fn restore_annotations(&mut self, mut earlier: Vec<String>) {
        earlier.append(&mut self.annotations);
        self.annotations = earlier;
    }

    /// Quote an earlier response into the pending message, reporting whether
    /// there was anything to quote.
    pub(crate) fn add_annotation(&mut self, text: String) -> bool {
        let text = text.trim().to_string();

        if text.is_empty() {
            return false;
        }

        self.annotations.push(text);

        true
    }

    /// Take one annotation back off the pending message. The rest keep their
    /// order, so the numbers the remaining chips carry stay the numbers the
    /// prompt will send them under.
    pub(crate) fn remove_annotation(&mut self, index: usize) -> bool {
        if index >= self.annotations.len() {
            return false;
        }

        self.annotations.remove(index);

        true
    }

    /// Attach a decoded image and write its placeholder at the cursor, so the
    /// text keeps the record of where the image belongs.
    pub(crate) fn attach_image(
        &mut self,
        image: &Image,
        input: &Entity<TextareaState>,
        window: &mut Window,
        cx: &mut Context<AgentPane>,
    ) -> Result<(), AttachError> {
        let placeholder = self.images.attach(image)?;

        input.update(cx, |input, cx| {
            let preceding = input.text().chars_at(input.cursor()).prev();

            input.insert(spaced_placeholder(preceding, &placeholder), window, cx);
        });

        Ok(())
    }

    /// Drop the attachment at `index` by deleting its placeholder, then let
    /// reconciliation renumber what is left. Removal and a hand-edited
    /// deletion therefore take the same path.
    pub(crate) fn remove_image(
        &mut self,
        index: usize,
        input: &Entity<TextareaState>,
        window: &mut Window,
        cx: &mut Context<AgentPane>,
    ) -> bool {
        let Some(placeholder) = self.images.placeholder_at(index) else {
            return false;
        };

        let text = input.read(cx).text().to_string();
        let without = text.replace(placeholder, "");

        input.update(cx, |input, cx| input.set_value(without.clone(), window, cx));

        self.sync(&without, input, window, cx)
    }

    /// Bring the attachment list back in line with the composer text. The text
    /// is the record of which images the message still carries, so this runs
    /// after every edit that could have changed its placeholders. Reports
    /// whether the strip needs redrawing.
    pub(crate) fn sync(
        &mut self,
        text: &str,
        input: &Entity<TextareaState>,
        window: &mut Window,
        cx: &mut Context<AgentPane>,
    ) -> bool {
        if self.images.is_empty() {
            input.update(cx, |input, cx| input.set_links(Vec::new(), cx));

            return false;
        }

        // Reconciliation is the one place the placeholders settle, so the
        // links are marked from here: every route that can move them --
        // typing, deleting a thumbnail, renumbering -- ends up here, and the
        // ranges are read from the text the input is left holding.
        let text = match self.images.reconcile(text) {
            Some(renumbered) => {
                // Renumbering rewrites digits in place, and a message carries
                // fewer than ten images, so every placeholder keeps its length
                // and the caret still belongs where the edit left it. Setting
                // the value drops the selection, so it is put back rather than
                // letting an edit in the middle of a prompt throw the caret to
                // the top of the composer.
                let cursor = input.read(cx).cursor();

                input.update(cx, |input, cx| {
                    input.set_value(renumbered.clone(), window, cx);
                    input.set_selected_range(cursor..cursor, cx);
                });

                renumbered
            }

            None => text.to_string(),
        };

        let links = self.images.placeholder_links(&text);

        input.update(cx, |input, cx| input.set_links(links, cx));

        true
    }
}

pub(crate) use nmt_agent::images::{AttachError, MAX_ATTACHMENTS};
#[cfg(test)]
use nmt_agent::images::{MAX_IMAGE_EDGE, placeholder_text};

pub(crate) struct Attachment<'a>(&'a CoreAttachment<Arc<Image>>);

impl<'a> Attachment<'a> {
    pub(crate) fn bytes(&self) -> &'a [u8] {
        self.0.image.bytes()
    }

    pub(crate) fn format(&self) -> ImageFormat {
        self.0.image.format()
    }

    pub(crate) fn placeholder(&self) -> &'a str {
        self.0.placeholder()
    }

    pub(crate) fn image(&self) -> Arc<Image> {
        self.0.image.clone()
    }
}

#[derive(Default)]
pub(crate) struct PendingAttachments(CorePendingAttachments<Arc<Image>>);

impl PendingAttachments {
    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = Attachment<'_>> {
        self.0.iter().map(Attachment)
    }

    pub(crate) fn clear(&mut self) {
        self.0.clear();
    }

    pub(crate) fn attach(&mut self, image: &Image) -> Result<String, AttachError> {
        self.0.attach(image.bytes(), |bytes| {
            Arc::new(Image::from_bytes(ImageFormat::Png, bytes))
        })
    }

    pub(crate) fn placeholder_links(&self, text: &str) -> Vec<Range<usize>> {
        self.0.placeholder_links(text)
    }

    pub(crate) fn linked_image(&self, text: &str, range: Range<usize>) -> Option<Arc<Image>> {
        self.0.linked_image(text, range).cloned()
    }

    pub(crate) fn placeholder_at(&self, index: usize) -> Option<&str> {
        self.0.placeholder_at(index)
    }

    pub(crate) fn reconcile(&mut self, text: &str) -> Option<String> {
        self.0.reconcile(text)
    }
}

/// Where a pane writes the attachment files a harness reads by path. Keyed by
/// the pane's route so two panes cannot collide, and removed with the pane, so
/// nothing outlives the tab that pasted it.
pub(crate) fn scratch_dir(route: &str) -> PathBuf {
    let key: String = route
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();

    env::temp_dir().join(format!("niumaterm-agent-{key}"))
}

#[cfg(test)]
mod tests;
