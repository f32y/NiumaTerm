//! Frosted layers that arrive and leave over time.
//!
//! An Agent pane puts a blurred layer over its content in a few places: to
//! hold the pane while its backend is away, to push the transcript back under
//! the `/resume` list, to show an image over the conversation it came from.
//! Each of them switching on and off between two frames reads as a glitch,
//! so they share one ramp and one element.

use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{AnyElement, App, ClickEvent, IntoElement, Pixels, RenderOnce, Window, div, px};
use gpui_component::ActiveTheme as _;

use crate::settings::AgentSettings;

/// Eased position along a transition, for a parameter already clamped to
/// `0..=1`. The ramp leaves and arrives at zero speed, so neither end of a
/// transition built on it reads as the effect being switched on.
fn smoothstep(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

/// A `0..=1` ramp between two states of an effect: where it started, what it
/// is heading for, and when it left. Reversing mid-ramp starts a fresh one from
/// wherever the previous had reached, so an effect dismissed while it is still
/// arriving retreats from the value actually on screen instead of snapping to
/// full first.
#[derive(Clone, Copy)]
pub(crate) struct Fade {
    from: f32,
    to: f32,
    start: Instant,
}

impl Fade {
    const DURATION: Duration = Duration::from_millis(150);

    /// The frame's worth of a layer that is up when `open` and away
    /// otherwise, asking for another frame while the ramp is still
    /// travelling. Called from a render, so the notify that produced this
    /// frame already woke the pump; the next-frame request keeps it awake
    /// until the ramp settles.
    ///
    /// The ramp is retargeted here rather than where the layer is shown and
    /// hidden, so every path in and out of it animates without each having
    /// to remember to. Under reduced motion it is still retargeted, so a
    /// layer dismissed while motion is on and reopened after it is off
    /// resumes from what is on screen; only the travel is skipped.
    pub(crate) fn drive(
        &mut self,
        open: bool,
        now: Instant,
        window: &mut Window,
        cx: &App,
    ) -> FadeFrame {
        let to = if open { 1.0 } else { 0.0 };
        if self.to != to {
            *self = Self {
                from: self.progress(now),
                to,
                start: now,
            };
        }

        let opacity = if cx.global::<AgentSettings>().reduce_motion {
            to
        } else {
            if now.duration_since(self.start) < Self::DURATION {
                window.request_animation_frame();
            }
            self.progress(now)
        };

        FadeFrame { open, opacity }
    }

    fn progress(&self, now: Instant) -> f32 {
        let elapsed = now.duration_since(self.start).as_secs_f32();
        let t = (elapsed / Self::DURATION.as_secs_f32()).clamp(0.0, 1.0);

        self.from + (self.to - self.from) * smoothstep(t)
    }
}

impl Default for Fade {
    fn default() -> Self {
        Self {
            from: 0.0,
            to: 0.0,
            start: Instant::now(),
        }
    }
}

/// One frame of a fading layer: whether it is up, and how much of it shows.
/// The two are separate because a layer on its way out still shows.
#[derive(Clone, Copy)]
pub(crate) struct FadeFrame {
    open: bool,
    opacity: f32,
}

impl FadeFrame {
    /// Nothing of the layer is on screen: it can be left out of the frame,
    /// and whatever was kept for its fade-out can go.
    pub(crate) fn gone(self) -> bool {
        !self.open && self.opacity <= 0.0
    }
}

type ClickListener = Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// A blurred, tinted layer over the whole of its parent, at the point along
/// its fade that `FadeFrame` describes.
///
/// The layer fades as a whole rather than by blur radius: the renderer
/// composites a backdrop blur as a lerp from the sharp backdrop to the
/// blurred one by element opacity, which crosses smoothly, while its
/// reduction pass puts a floor under small radii that shows as a jump on a
/// radius ramp's first frame.
///
/// The layer takes the pointer only while it is up. One still eating clicks
/// as it fades, after whatever it covered for has ended, would read as the
/// pane having hung.
#[derive(IntoElement)]
pub(crate) struct FrostedLayer {
    frame: FadeFrame,
    blur: Pixels,
    tint: f32,
    padded: bool,
    on_click: Option<ClickListener>,
    children: Vec<AnyElement>,
}

impl FrostedLayer {
    pub(crate) fn new(frame: FadeFrame) -> Self {
        Self {
            frame,
            blur: px(24.),
            tint: 0.45,
            padded: false,
            on_click: None,
            children: Vec::new(),
        }
    }

    /// A lighter frost, for a layer that pushes content back rather than
    /// covering it.
    pub(crate) fn light(mut self) -> Self {
        self.blur = px(16.);
        self.tint = 0.25;
        self
    }

    /// Room between the children and narrow pane edges, for content that
    /// wraps.
    pub(crate) fn padded(mut self) -> Self {
        self.padded = true;
        self
    }

    /// What a click on the layer itself does while it is up.
    pub(crate) fn on_click(
        mut self,
        listener: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Box::new(listener));
        self
    }
}

impl ParentElement for FrostedLayer {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for FrostedLayer {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        div()
            .id("frosted-layer")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .when(self.frame.open, |this| this.occlude())
            .when_some(
                self.on_click.filter(|_| self.frame.open),
                |this, on_click| this.on_click(on_click),
            )
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .when(self.padded, |this| this.p_6())
            .opacity(self.frame.opacity)
            .backdrop_blur(self.blur)
            .bg(cx.theme().background.opacity(self.tint))
            .children(self.children)
    }
}
