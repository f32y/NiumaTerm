use gpui::prelude::*;
use gpui::{AnyElement, App, IntoElement, RenderOnce, Window, px};
use gpui_component::{ActiveTheme as _, v_flex};

/// A modal layer over an Agent pane while its backend cannot accept input.
/// Callers supply the state-specific body and keep ownership of all commands.
#[derive(IntoElement)]
pub(in crate::view) struct BlockingOverlay {
    body: AnyElement,
    padded: bool,
}

impl BlockingOverlay {
    pub(in crate::view) fn new(body: impl IntoElement) -> Self {
        Self {
            body: body.into_any_element(),
            padded: false,
        }
    }

    /// Failure content needs room from narrow pane edges; compact progress
    /// content does not, so padding is opt-in at the call site.
    pub(in crate::view) fn padded(mut self) -> Self {
        self.padded = true;
        self
    }
}

impl RenderOnce for BlockingOverlay {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        v_flex()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .occlude()
            .items_center()
            .justify_center()
            .when(self.padded, |this| this.p_6())
            .backdrop_blur(px(24.))
            .bg(cx.theme().background.opacity(0.45))
            .child(self.body)
    }
}
