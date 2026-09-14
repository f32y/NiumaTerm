use gpui::prelude::*;
use gpui::{App, Div};
use gpui_component::{ActiveTheme as _, h_flex, v_flex};

use crate::design::CARD_RADIUS;

/// Ordinary and Team conversations share the input card so font and spacing
/// changes stay aligned with the transcript in both views.
pub(crate) fn composer_card(cx: &App) -> Div {
    v_flex()
        .w_full()
        .rounded(CARD_RADIUS)
        .overflow_hidden()
        .border_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().popover)
        .shadow_sm()
}

pub(crate) fn composer_input_row() -> Div {
    h_flex().w_full().px_4().pt_3().pb_1()
}

pub(crate) fn composer_controls_row() -> Div {
    h_flex()
        .w_full()
        .px_3()
        .pb_2()
        .pt_0p5()
        .items_center()
        .gap_2()
}
