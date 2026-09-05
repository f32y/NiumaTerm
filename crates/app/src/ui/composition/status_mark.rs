use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    Animation, AnimationExt as _, App, ElementId, IntoElement, Pixels, RenderOnce, SharedString,
    Window, div, ease_in_out,
};
use gpui_component::progress::ProgressCircle;
use gpui_component::{ActiveTheme as _, Sizable as _};

/// One breath of a pulsing mark, and how far its opacity travels. Slow enough
/// to read as ongoing work rather than as a blinking alert.
const PULSE_PERIOD: Duration = Duration::from_millis(1_600);
const PULSE_MIN_OPACITY: f32 = 0.35;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StatusMarkTone {
    Warning,
    Success,
}

enum StatusMarkVisual {
    Dot { tone: StatusMarkTone, size: Pixels },
    Busy,
}

/// A fixed-size semantic status dot. The caller owns state precedence and may
/// attach wording when the surrounding control does not already name the mark.
#[derive(IntoElement)]
pub(crate) struct StatusMark {
    id: ElementId,
    visual: StatusMarkVisual,
    label: Option<SharedString>,
    pulse: bool,
}

impl StatusMark {
    pub(crate) fn new(id: impl Into<ElementId>, tone: StatusMarkTone, size: Pixels) -> Self {
        Self {
            id: id.into(),
            visual: StatusMarkVisual::Dot { tone, size },
            label: None,
            pulse: false,
        }
    }

    pub(crate) fn busy(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            visual: StatusMarkVisual::Busy,
            label: None,
            pulse: false,
        }
    }

    pub(crate) fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Breathe the mark, for a dot that reports work still in flight. A dot
    /// says what the state is; the pulse is what says it is still moving,
    /// where a row has no room for a spinner beside its label.
    pub(crate) fn pulse(mut self) -> Self {
        self.pulse = true;
        self
    }
}

impl RenderOnce for StatusMark {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        match self.visual {
            StatusMarkVisual::Dot { tone, size } => {
                let mark = div()
                    .id(self.id.clone())
                    .flex_none()
                    .size(size)
                    .rounded_full()
                    .when_some(self.label, |this, label| this.aria_label(label));
                let mark = match tone {
                    StatusMarkTone::Warning => mark.bg(cx.theme().warning),
                    StatusMarkTone::Success => mark.bg(cx.theme().success),
                };

                if !self.pulse {
                    return mark.into_any_element();
                }

                mark.with_animation(
                    self.id,
                    Animation::new(PULSE_PERIOD).repeat(),
                    |mark, delta| {
                        // One breath per period: the ramp turns at the
                        // halfway point rather than snapping back to full.
                        let phase = 1.0 - (delta * 2.0 - 1.0).abs();

                        mark.opacity(
                            PULSE_MIN_OPACITY + (1.0 - PULSE_MIN_OPACITY) * ease_in_out(phase),
                        )
                    },
                )
                .into_any_element()
            }
            StatusMarkVisual::Busy => ProgressCircle::new(self.id)
                .small()
                .loading(true)
                .color(cx.theme().warning)
                .into_any_element(),
        }
    }
}
