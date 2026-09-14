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

pub(crate) use nmt_agent::images::{AttachError, MAX_ATTACHMENTS};

#[cfg(test)]
mod tests;

use crate::agent_tab::AgentPane;
use crate::agent_tab::settings::UI_RADIUS;
use gpui::prelude::*;
use gpui::{
    AnyElement, Bounds, Context, Entity, FontWeight, Image, ImageFormat, ObjectFit, SharedString,
    Window, div, img, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::TextareaState;
use gpui_component::tooltip::Tooltip;
use gpui_component::{ActiveTheme as _, ElementExt as _, IconName, Sizable as _, h_flex, v_flex};
#[cfg(test)]
use nmt_agent::images::MAX_IMAGE_EDGE;
#[cfg(test)]
use nmt_agent::images::placeholder_text;
use nmt_agent::images::{Attachment, PendingAttachments as CorePendingAttachments};
use rust_i18n::t;
use std::cell::Cell;
use std::env;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

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
        let placeholder = attach_png(&mut self.images, image)?;

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

    /// The images the pending message carries, above the composer text they
    /// are anchored in. Absent while nothing is attached, so an ordinary
    /// message keeps the composer where it has always been.
    pub(crate) fn render(&self, cx: &mut Context<AgentPane>) -> Option<AnyElement> {
        if self.images().is_empty() && self.annotations().is_empty() {
            return None;
        }

        Some(
            h_flex()
                .id("agent-attachments")
                .aria_label(t!("agent-composer-context-label"))
                .w_full()
                .px_3()
                .pt_3()
                .gap_2()
                .flex_wrap()
                .children(
                    self.images()
                        .iter()
                        .enumerate()
                        .map(|(index, attachment)| self.render_attachment(index, attachment, cx)),
                )
                .children(
                    self.annotations()
                        .iter()
                        .enumerate()
                        .map(|(index, text)| self.render_response_annotation(index, text, cx)),
                )
                .into_any_element(),
        )
    }

    /// One thumbnail with the control that takes it back off. The image
    /// renders from the bytes the paste produced, so no file is written for
    /// something the user may still remove.
    fn render_attachment(
        &self,
        index: usize,
        attachment: &Attachment<Arc<Image>>,
        cx: &mut Context<AgentPane>,
    ) -> AnyElement {
        let image = attachment.image.clone();

        // A click carries the pointer's position, not the thumbnail's; the
        // bounds the layout gave it are kept from the prepaint that precedes
        // the click, so the preview knows where to grow from.
        let placed = Rc::new(Cell::new(Bounds::default()));

        div()
            .id(("agent-attachment", index))
            .group("agent-attachment")
            .relative()
            .size(px(THUMBNAIL))
            .flex_none()
            .rounded(UI_RADIUS)
            .overflow_hidden()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().muted)
            .aria_label(attachment.placeholder().to_string())
            // A thumbnail is cropped to a square this small, so opening it is
            // the only way to check what is about to be sent.
            .cursor_pointer()
            .on_prepaint({
                let placed = placed.clone();

                move |bounds, _, _| placed.set(bounds)
            })
            .on_click(cx.listener({
                let image = image.clone();

                move |this, _, _, cx| this.open_image(image.clone(), Some(placed.get()), cx)
            }))
            .child(img(image).size_full().object_fit(ObjectFit::Cover))
            .child(
                div()
                    .absolute()
                    .top_0()
                    .right_0()
                    .invisible()
                    .group_hover("agent-attachment", |this| this.visible())
                    .child(
                        Button::new(("agent-attachment-remove", index))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Close)
                            .accessibility_label(t!("agent-composer-image-remove"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                // The control sits on the thumbnail, which
                                // opens the image; taking the image off is not
                                // a request to look at it.
                                cx.stop_propagation();

                                this.remove_attachment(index, window, cx)
                            })),
                    ),
            )
            .into_any_element()
    }

    /// One annotation, as its own chip. They stay separate rather than folding
    /// into a single count because each one is a different quotation the user
    /// chose, and one of them being wrong is a reason to drop that one.
    ///
    /// The chip shows as much as fits and carries the whole selection in its
    /// tooltip: a quotation is often several lines, and a strip that grew to
    /// hold them would take the composer's room.
    fn render_response_annotation(
        &self,
        index: usize,
        text: &str,
        cx: &mut Context<AgentPane>,
    ) -> AnyElement {
        let mut chars = text.chars();
        let mut preview: String = chars.by_ref().take(ANNOTATION_PREVIEW_CHARS).collect();

        if chars.next().is_some() {
            preview.push('…');
        }

        let label = t!("agent-composer-annotation-item", index = (index + 1)).into_owned();

        let group: SharedString = format!("agent-response-annotation-{index}").into();
        let full: SharedString = text.to_string().into();

        div()
            .id(("agent-response-annotation", index))
            .group(group.clone())
            .relative()
            .w(px(ANNOTATION_WIDTH))
            .max_w_full()
            .h(px(THUMBNAIL))
            .flex_none()
            .rounded(UI_RADIUS)
            .overflow_hidden()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().muted)
            .aria_label(format!("{label}: {text}"))
            .tooltip(move |window, cx| Tooltip::new(full.clone()).build(window, cx))
            .child(
                v_flex()
                    .size_full()
                    .px_2()
                    .py_1p5()
                    .pr_7()
                    .overflow_hidden()
                    .gap_0p5()
                    .child(div().text_xs().font_weight(FontWeight::MEDIUM).child(label))
                    .child(
                        div()
                            .truncate()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(preview),
                    ),
            )
            .child(
                div()
                    .absolute()
                    .top_0()
                    .right_0()
                    .invisible()
                    .group_hover(group, |this| this.visible())
                    .child(
                        Button::new(("agent-response-annotation-remove", index))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Close)
                            .accessibility_label(t!("agent-composer-annotations-remove"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.remove_response_annotation(index, cx)
                            })),
                    ),
            )
            .into_any_element()
    }
}

pub(crate) type PendingAttachments = CorePendingAttachments<Arc<Image>>;

fn attach_png(pending: &mut PendingAttachments, image: &Image) -> Result<String, AttachError> {
    pending.attach(image.bytes(), |bytes| {
        Arc::new(Image::from_bytes(ImageFormat::Png, bytes))
    })
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

/// Edge of a thumbnail. Large enough to recognize a screenshot by, small
/// enough that a full message's worth of them does not push the composer off
/// the pane.
pub(crate) const THUMBNAIL: f32 = 56.0;

const ANNOTATION_WIDTH: f32 = 240.0;

const ANNOTATION_PREVIEW_CHARS: usize = 160;
