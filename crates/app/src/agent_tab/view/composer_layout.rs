use gpui::prelude::*;
use gpui::{App, Div, px};
use gpui_component::{ActiveTheme as _, h_flex, v_flex};

/// Ordinary and Team conversations share the input card so font and spacing
/// changes stay aligned with the transcript in both views.
pub(crate) fn composer_card(cx: &App) -> Div {
    v_flex()
        .w_full()
        .rounded(px(16.))
        .overflow_hidden()
        .border_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().popover)
        .shadow_md()
}

pub(crate) fn composer_input_row() -> Div {
    h_flex().w_full().px(px(10.)).pt_3().pb_1()
}

pub(crate) fn composer_controls_row() -> Div {
    h_flex()
        .w_full()
        .px(px(10.))
        .pb_2()
        .pt_0p5()
        .items_center()
        .gap_2()
}
