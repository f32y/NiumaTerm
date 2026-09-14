use app::design::TAB_HEIGHT;
use gpui::prelude::*;
use gpui::{Context, Render, SharedString, Window, div, px};
use gpui_component::{ActiveTheme as _, h_flex};

use crate::ui::UI_RADIUS;

pub(super) struct TabDrag {
    pub(super) from: usize,
}

/// Full-size tab pill shown under the pointer during a reorder drag.
pub(crate) struct DragLabelPreview {
    pub(crate) style: DragStyle,
    pub(crate) label: SharedString,
    pub(crate) width: f32,
}

pub(crate) enum DragStyle {
    Tab,
    Sidebar,
}

/// Height of a tab row. A workspace item stacks a name and a path line, so a
/// row stays visibly shorter than one and the two tiers read as ranked, while
/// leaving the row a comfortable click target.
pub(crate) const TAB_ROW_HEIGHT: f32 = 28.0;

impl Render for DragLabelPreview {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        match self.style {
            DragStyle::Tab => {
                // The chrome background may be translucent, while a drag preview floats
                // over unrelated content and therefore needs an opaque compositing base.
                div()
                    .rounded(UI_RADIUS)
                    .bg(cx.theme().background.alpha(1.0))
                    .child(
                        div()
                            .w(px(self.width))
                            .h(TAB_HEIGHT)
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
            DragStyle::Sidebar => h_flex()
                .w(px(self.width))
                .h(px(TAB_ROW_HEIGHT))
                .px_2()
                .items_center()
                .rounded(UI_RADIUS)
                .overflow_hidden()
                .text_xs()
                .bg(cx
                    .theme()
                    .background
                    .blend(cx.theme().sidebar)
                    .blend(cx.theme().sidebar_accent))
                .text_color(cx.theme().sidebar_accent_foreground)
                .child(div().truncate().child(self.label.clone())),
        }
    }
}
