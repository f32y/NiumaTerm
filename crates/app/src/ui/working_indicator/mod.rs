use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    Animation, AnimationExt as _, App, ElementId, Hsla, IntoElement, RenderOnce, Window, div,
    ease_in_out, px,
};
use gpui_component::h_flex;

const DOT_COUNT: usize = 3;
const CYCLE_DURATION: Duration = Duration::from_millis(1_100);
const DOT_CELL_SIZE: f32 = 4.0;
const DOT_MIN_SIZE: f32 = 3.2;
const DOT_MIN_OPACITY: f32 = 0.28;
const DOT_MAX_OPACITY: f32 = 0.88;

/// Three pulsing dots for an ongoing operation with no measurable completion.
#[derive(IntoElement)]
pub(crate) struct WorkingIndicator {
    color: Hsla,
}

impl WorkingIndicator {
    pub(crate) fn new(color: Hsla) -> Self {
        Self { color }
    }
}

impl RenderOnce for WorkingIndicator {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let color = self.color;

        h_flex().gap_1().children((0..DOT_COUNT).map(move |index| {
            // A fixed cell prevents the size pulse from shifting adjacent dots.
            div()
                .flex()
                .size(px(DOT_CELL_SIZE))
                .items_center()
                .justify_center()
                .child(div().rounded_full().bg(color).with_animation(
                    ElementId::NamedInteger("working-indicator-dot".into(), index as u64),
                    Animation::new(CYCLE_DURATION).repeat(),
                    move |dot, delta| {
                        let pulse = dot_pulse(delta, index);
                        let size = DOT_MIN_SIZE + (DOT_CELL_SIZE - DOT_MIN_SIZE) * pulse;
                        let opacity = DOT_MIN_OPACITY + (DOT_MAX_OPACITY - DOT_MIN_OPACITY) * pulse;

                        dot.size(px(size)).opacity(opacity)
                    },
                ))
        }))
    }
}

fn dot_pulse(delta: f32, index: usize) -> f32 {
    let interval = 1.0 / DOT_COUNT as f32;
    let phase = (delta - index as f32 * interval).rem_euclid(1.0);
    let distance = phase.min(1.0 - phase);
    let pulse = (1.0 - distance / interval).clamp(0.0, 1.0);
    ease_in_out(pulse)
}

#[cfg(test)]
mod tests;
