use gpui::prelude::*;
use gpui::{Context, Render, SharedString, Window, div, px};
use gpui_component::ActiveTheme as _;

use crate::ui::UI_RADIUS;

pub(super) struct TabDrag {
    pub(super) from: usize,
}

/// Full-size tab pill shown under the pointer during a reorder drag.
pub(super) struct TabDragPreview {
    pub(super) label: SharedString,
    pub(super) width: f32,
}

impl Render for TabDragPreview {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The chrome background may be translucent, while a drag preview floats
        // over unrelated content and therefore needs an opaque compositing base.
        div()
            .rounded(UI_RADIUS)
            .bg(cx.theme().background.alpha(1.0))
            .child(
                div()
                    .w(px(self.width))
                    .h(px(30.0))
                    .px_2()
                    .flex()
                    .items_center()
                    .justify_center()
                    .overflow_hidden()
                    .rounded(UI_RADIUS)
                    .bg(cx.theme().tab_active)
                    .text_sm()
                    .text_color(cx.theme().tab_active_foreground)
                    .child(div().truncate().child(self.label.clone())),
            )
    }
}
