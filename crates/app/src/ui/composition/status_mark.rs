use gpui::prelude::*;
use gpui::{App, ElementId, IntoElement, Pixels, RenderOnce, SharedString, Window, div};
use gpui_component::progress::ProgressCircle;
use gpui_component::{ActiveTheme as _, Sizable as _};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StatusMarkTone {
    Primary,
    Warning,
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
}

impl StatusMark {
    pub(crate) fn new(id: impl Into<ElementId>, tone: StatusMarkTone, size: Pixels) -> Self {
        Self {
            id: id.into(),
            visual: StatusMarkVisual::Dot { tone, size },
            label: None,
        }
    }

    pub(crate) fn busy(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            visual: StatusMarkVisual::Busy,
            label: None,
        }
    }

    pub(crate) fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }
}

impl RenderOnce for StatusMark {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        match self.visual {
            StatusMarkVisual::Dot { tone, size } => {
                let color = match tone {
                    StatusMarkTone::Primary => cx.theme().primary,
                    StatusMarkTone::Warning => cx.theme().warning,
                };

                div()
                    .id(self.id)
                    .flex_none()
                    .size(size)
                    .rounded_full()
                    .bg(color)
                    .when_some(self.label, |this, label| this.aria_label(label))
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
