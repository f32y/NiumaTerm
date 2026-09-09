use gpui::prelude::*;
use gpui::{AnyElement, App, IntoElement, RenderOnce, Window, px};
use gpui_component::{ActiveTheme as _, v_flex};

/// A modal layer over an Agent pane while its backend cannot accept input.
/// Callers supply the state-specific body and keep ownership of all commands.
#[derive(IntoElement)]
pub(super) struct BlockingOverlay {
    /// Absent while the layer is fading out after the backend came back: the
    /// body belonged to a state that no longer holds, so only the frosted
    /// layer itself lingers for the length of the fade.
    body: Option<AnyElement>,
    padded: bool,
    opacity: f32,
}

impl BlockingOverlay {
    pub(super) fn new(body: impl IntoElement) -> Self {
        Self {
            body: Some(body.into_any_element()),
            padded: false,
            opacity: 1.0,
        }
    }

    /// The bare layer with nothing on it, for the frames after the backend
    /// came back while the blur is still fading.
    pub(super) fn fading() -> Self {
        Self {
            body: None,
            padded: false,
            opacity: 1.0,
        }
    }

    /// Failure content needs room from narrow pane edges; compact progress
    /// content does not, so padding is opt-in at the call site.
    pub(super) fn padded(mut self) -> Self {
        self.padded = true;
        self
    }

    /// Where along its fade the layer is. The renderer composites a backdrop
    /// blur as a lerp between the sharp backdrop and the blurred one by the
    /// element's opacity, so fading the whole layer crosses from sharp to
    /// frosted smoothly; ramping the blur radius instead would jump at the
    /// low end, where the renderer's reduction pass sets a floor on the blur.
    pub(super) fn opacity(mut self, opacity: f32) -> Self {
        self.opacity = opacity;
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
            // Swallows input only while there is a state to hold it for; a
            // fading layer still eating clicks after the backend came back
            // would read as the tab having hung.
            .when(self.body.is_some(), |this| this.occlude())
            .items_center()
            .justify_center()
            .when(self.padded, |this| this.p_6())
            .opacity(self.opacity)
            .backdrop_blur(px(24.))
            .bg(cx.theme().background.opacity(0.45))
            .children(self.body)
    }
}
