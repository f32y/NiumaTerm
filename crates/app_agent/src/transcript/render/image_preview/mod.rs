//! An image from the conversation, opened over it at full size.
//!
//! The transcript shows attachments as thumbnails, which is enough to
//! recognize a screenshot but not to read one. Opening it here keeps the
//! reader in the conversation: the layer covers the message stream it came
//! from, blurred rather than replaced, so the surrounding messages still mark
//! where the image belongs.

use std::sync::Arc;

use gpui::prelude::*;
use gpui::{
    AnyElement, Context, Image, MouseButton, ObjectFit, Pixels, Size, Window, div, img, px, size,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{ActiveTheme as _, IconName};
use nmt_i18n::i18n;

use crate::transcript::TranscriptView;

/// Share of the message stream an enlarged image may take. Short of the whole
/// area so the blurred conversation stays visible around it, which is what
/// makes the image read as a layer over the transcript rather than as another
/// screen.
const PREVIEW_FRACTION: f32 = 0.8;
/// Edge of the round close control, and how far it hangs past the image's
/// corner. Straddling the corner keeps it clear of the image's own content,
/// which is what the reader opened the image to see.
const PREVIEW_CLOSE_EDGE: f32 = 28.0;
const PREVIEW_CLOSE_OFFSET: f32 = 10.0;

impl TranscriptView {
    pub(crate) fn zoom_image(&mut self, image: Arc<Image>, cx: &mut Context<Self>) {
        self.zoomed_image = Some(image);
        cx.notify();
    }

    pub(crate) fn close_zoomed_image(&mut self, cx: &mut Context<Self>) {
        if self.zoomed_image.take().is_some() {
            cx.notify();
        }
    }

    /// The enlarged image and the mask under it, while one is open.
    pub(crate) fn render_zoomed_image(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let image = self.zoomed_image.clone()?;

        Some(
            div()
                .id("agent-transcript-image-preview")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                // Takes the pointer for the whole message stream: the
                // conversation underneath is context for the image now, so
                // clicking it dismisses the image rather than acting on the
                // row that happens to be under the pointer.
                .occlude()
                .flex()
                .items_center()
                .justify_center()
                .backdrop_blur(px(24.))
                .bg(cx.theme().background.opacity(0.45))
                .on_click(cx.listener(|this, _, _, cx| this.close_zoomed_image(cx)))
                .children(self.render_preview_image(image, window, cx))
                .into_any_element(),
        )
    }

    /// The image itself, at the size the transcript has room for, with the
    /// control that closes it. Absent until the decoder has the image: its
    /// pixel dimensions are what the layout is built from, and the loading
    /// pass notifies this view when they arrive.
    fn render_preview_image(
        &self,
        image: Arc<Image>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        // The image element takes its size from the style rather than from the
        // pixels, so the decoded frame is the only place the image's own
        // dimensions can come from.
        let frame = image.clone().use_render_image(window, cx)?.size(0);
        // Decoded dimensions count device pixels, so dividing by the display
        // scale spends one screen pixel per image pixel: the image is shown as
        // sharp as it is, without the display's scale magnifying it.
        let scale = window.scale_factor();
        let natural = size(
            px(frame.width.0 as f32 / scale),
            px(frame.height.0 as f32 / scale),
        );
        let room = size(self.transcript_width?, self.transcript_height?);
        let shown = preview_size(natural, room);

        Some(
            div()
                .relative()
                .w(shown.width)
                .h(shown.height)
                // The mask closes on click, and the image is not part of what
                // the reader is dismissing; claiming the press keeps a click
                // on the image itself from reaching it.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(img(image).size_full().object_fit(ObjectFit::Contain))
                .child(
                    div()
                        .absolute()
                        .top(px(-PREVIEW_CLOSE_OFFSET))
                        .right(px(-PREVIEW_CLOSE_OFFSET))
                        .child(
                            Button::new("agent-transcript-image-close")
                                .secondary()
                                .size(px(PREVIEW_CLOSE_EDGE))
                                .rounded_full()
                                .icon(IconName::Close)
                                .tooltip(i18n("agent-transcript-image-close"))
                                .aria_label(i18n("agent-transcript-image-close"))
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.close_zoomed_image(cx)),
                                ),
                        ),
                )
                .into_any_element(),
        )
    }
}

/// What an image is shown at: its own size while the message stream has room
/// for it, otherwise scaled down to the share of that room a preview may take.
/// One factor applies to both edges, so a shrunk image keeps its shape.
fn preview_size(natural: Size<Pixels>, room: Size<Pixels>) -> Size<Pixels> {
    if natural.width <= px(0.) || natural.height <= px(0.) {
        return natural;
    }

    let factor = (room.width * PREVIEW_FRACTION / natural.width)
        .min(room.height * PREVIEW_FRACTION / natural.height)
        .min(1.0);
    size(natural.width * factor, natural.height * factor)
}

#[cfg(test)]
mod tests;
