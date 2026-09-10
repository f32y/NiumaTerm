use gpui::prelude::*;
use gpui::{AnyElement, App, div, px};
use gpui_component::{ActiveTheme as _, v_flex};

pub(crate) fn empty_state(title: &str, detail: &str, cx: &App) -> AnyElement {
    v_flex()
        .flex_1()
        .items_center()
        .justify_center()
        .gap_1()
        .px(px(16.0))
        .child(div().text_sm().child(title.to_string()))
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(detail.to_string()),
        )
        .into_any_element()
}
