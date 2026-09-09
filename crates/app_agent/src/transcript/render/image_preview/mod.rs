//! An image from the conversation, opened over it at full size.
//!
//! The transcript shows attachments as thumbnails, which is enough to
//! recognize a screenshot but not to read one. Opening it here keeps the
//! reader in the conversation: the layer covers the message stream it came
//! from, blurred rather than replaced, so the surrounding messages still mark
//! where the image belongs. The image grows out of the thumbnail that was
//! clicked and shrinks back into it, which is what ties the two together as
//! one picture rather than a thumbnail and an unrelated dialog.

use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{
    AnyElement, Bounds, Context, Image, MouseButton, ObjectFit, Pixels, Point, Size, Window,
    deferred, div, img, point, px, size,
};
use gpui_component::IconName;
use gpui_component::button::{Button, ButtonVariants as _};
use nmt_i18n::i18n;

use crate::fade::FrostedLayer;
use crate::settings::UI_RADIUS;
use crate::transcript::TranscriptView;

/// How long the image takes to travel between its thumbnail and its full
/// size. Brisk: the reader asked for the image and is waiting on it, and the
/// moving edges give the eye enough to follow even over a short span. Kept
/// as its own setting so the travel can be tuned apart from the fades.
pub(crate) const ZOOM_DURATION: Duration = Duration::from_millis(150);
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
    /// Open `image` over the conversation. `origin` is the thumbnail it was
    /// opened from, in window coordinates, for the image to grow out of; an
    /// image opened from something with no place on screen fades in where it
    /// ends up.
    pub(crate) fn zoom_image(
        &mut self,
        image: Arc<Image>,
        origin: Option<Bounds<Pixels>>,
        cx: &mut Context<Self>,
    ) {
        self.zoomed_image = Some(image);
        self.zoom_origin = origin;
        self.zoom_open = true;
        cx.notify();
    }

    pub(crate) fn close_zoomed_image(&mut self, cx: &mut Context<Self>) {
        if self.zoom_open {
            self.zoom_open = false;
            cx.notify();
        }
    }

    /// The mask over the conversation and the enlarged image above it, while
    /// one is open or still on its way back to its thumbnail. The image is a
    /// sibling of the mask rather than a child, so it stays solid while the
    /// mask underneath is still fading: a picture growing out of a thumbnail
    /// is what ties them together, and a ghost of one does not.
    pub(crate) fn render_zoomed_image(
        &mut self,
        now: Instant,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let frost = self.zoom_fade.drive(self.zoom_open, now, window, cx);
        // The image is released only once the fade-out has nothing left to
        // show; an opening layer also starts at zero and must keep it.
        if frost.gone() {
            self.zoomed_image = None;
        }
        let Some(image) = self.zoomed_image.clone() else {
            return Vec::new();
        };

        let mut elements = vec![
            FrostedLayer::new(frost)
                // The conversation underneath is context for the image now,
                // so clicking it dismisses the image rather than acting on
                // the row that happens to be under the pointer.
                .on_click(cx.listener(|this, _, _, cx| this.close_zoomed_image(cx)))
                .into_any_element(),
        ];
        elements.extend(
            self.render_preview_image(image, frost.progress(), window, cx)
                // A composer thumbnail sits in a sibling of the transcript
                // that paints after it, so an image growing out of one would
                // start behind the composer. Deferring the paint puts the
                // image over everything the pane draws in order, while its
                // layout stays in the transcript, where the viewport
                // coordinates it is placed in belong.
                .map(|image| deferred(image).into_any_element()),
        );
        elements
    }

    /// The image itself, part of the way from its thumbnail to the size the
    /// transcript has room for, with the control that closes it. Absent until
    /// the decoder has the image: its pixel dimensions are what the layout is
    /// built from, and the loading pass notifies this view when they arrive.
    fn render_preview_image(
        &self,
        image: Arc<Image>,
        progress: f32,
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
        let shown = Bounds::centered_at(
            point(room.width / 2.0, room.height / 2.0),
            preview_size(natural, room),
        );
        // The thumbnail reports where it is in the window; the image is laid
        // out inside the viewport, so it needs the same place measured from
        // the viewport's corner.
        let origin = self
            .zoom_origin
            .zip(self.transcript_origin)
            .map(|(origin, viewport)| Bounds {
                origin: origin.origin - viewport,
                size: origin.size,
            });
        let placed = preview_bounds(origin.unwrap_or(shown), shown, progress);

        Some(
            div()
                .absolute()
                .left(placed.origin.x)
                .top(placed.origin.y)
                .w(placed.size.width)
                .h(placed.size.height)
                // An image with nowhere to grow from fades in with the mask
                // instead of landing on it whole.
                .when(origin.is_none(), |this| this.opacity(progress))
                // The mask closes on click, and the image is not part of what
                // the reader is dismissing; claiming the press keeps a click
                // on the image itself from reaching it.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(
                    img(image)
                        .size_full()
                        // Cover throughout rather than Contain: the thumbnail
                        // is cropped to a square, and the image lands in a
                        // frame of its own shape, where Cover shows all of
                        // it. Contain would letterbox the crop on the first
                        // frame and snap it away on the last.
                        .object_fit(ObjectFit::Cover)
                        // The thumbnail's corners, straightened out as the
                        // image leaves it.
                        .rounded(UI_RADIUS * (1.0 - progress)),
                )
                .child(
                    div()
                        .absolute()
                        .top(px(-PREVIEW_CLOSE_OFFSET))
                        .right(px(-PREVIEW_CLOSE_OFFSET))
                        // The control belongs to the open image, so it arrives
                        // with it rather than riding on the corner of a
                        // thumbnail-sized picture.
                        .opacity(progress)
                        .child(
                            Button::new("agent-transcript-image-close")
                                .secondary()
                                .size(px(PREVIEW_CLOSE_EDGE))
                                .rounded_full()
                                .icon(IconName::Close)
                                .tooltip(i18n("agent-transcript-image-close"))
                                .accessibility_label(i18n("agent-transcript-image-close"))
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

/// Where the image is `progress` of the way from its thumbnail to its full
/// size. Every edge travels in step, so the frame changes shape as it grows
/// rather than growing first and reshaping after.
fn preview_bounds(from: Bounds<Pixels>, to: Bounds<Pixels>, progress: f32) -> Bounds<Pixels> {
    let between = |from: Pixels, to: Pixels| from + (to - from) * progress;

    Bounds {
        origin: Point {
            x: between(from.origin.x, to.origin.x),
            y: between(from.origin.y, to.origin.y),
        },
        size: Size {
            width: between(from.size.width, to.size.width),
            height: between(from.size.height, to.size.height),
        },
    }
}

#[cfg(test)]
mod tests;
