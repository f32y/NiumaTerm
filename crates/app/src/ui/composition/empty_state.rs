use gpui::prelude::*;
use gpui::{AnyElement, App, SharedString, div, px};
use gpui_component::{ActiveTheme as _, v_flex};

pub(crate) fn empty_state(
    title: impl Into<SharedString>,
    detail: impl Into<SharedString>,
    cx: &App,
) -> AnyElement {
    v_flex()
        .flex_1()
        .items_center()
        .justify_center()
        .gap_1()
        .px(px(16.0))
        .child(div().text_sm().child(title.into()))
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(detail.into()),
        )
        .into_any_element()
}
