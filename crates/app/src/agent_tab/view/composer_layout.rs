use gpui::prelude::*;
use gpui::{App, Div, ElementId, SharedString, px, relative};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::Enter;
use gpui_component::{ActiveTheme as _, Disableable as _, IconName, IconNamed, h_flex, v_flex};
use nmt_config::system::NewlineShortcut;
use rust_i18n::t;

use crate::agent_tab::settings::UI_RADIUS;
use crate::design::CARD_RADIUS;

/// Panels extend beneath the composer by this amount. Matching bottom padding
/// keeps their content above the card while its rounded edge covers the join.
pub(crate) const COMPOSER_PANEL_TUCK: f32 = 14.0;

pub(crate) fn composer_panel(cx: &App) -> Div {
    v_flex()
        .w(relative(0.95))
        .rounded_t(UI_RADIUS)
        .border_1()
        .border_b_0()
        .border_color(cx.theme().border.opacity(0.6))
        .bg(cx.theme().popover)
        .pb(px(COMPOSER_PANEL_TUCK))
}

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

/// Diameter of the send/stop control that closes the input line.
const COMPOSER_SEND_BUTTON: f32 = 32.0;

struct StopResponseIcon;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ComposerEnterBehavior {
    InsertNewline,
    Submit,
    ActivateOrSubmit,
}

/// What Enter does in a composer under the configured newline shortcut.
pub(crate) fn composer_enter_behavior(
    shortcut: NewlineShortcut,
    action: &Enter,
) -> ComposerEnterBehavior {
    match (action.secondary, action.shift) {
        (false, false) => ComposerEnterBehavior::ActivateOrSubmit,
        (true, false) if shortcut == NewlineShortcut::CtrlEnter => {
            ComposerEnterBehavior::InsertNewline
        }
        (false, true) if shortcut == NewlineShortcut::ShiftEnter => {
            ComposerEnterBehavior::InsertNewline
        }
        _ => ComposerEnterBehavior::Submit,
    }
}

impl IconNamed for StopResponseIcon {
    fn path(self) -> SharedString {
        "icons/stop.svg".into()
    }
}

/// The control at the trailing corner of a composer card: Send, or Stop in
/// its place while a turn runs. Callers attach what a click does, since the
/// two composers send and stop differently.
pub(crate) fn send_button(id: impl Into<ElementId>, running: bool, disabled: bool) -> Button {
    let label = match running {
        true => t!("agent-action-stop-response"),
        false => t!("agent-action-send-message"),
    };

    Button::new(id)
        .primary()
        .disabled(disabled)
        .size(px(COMPOSER_SEND_BUTTON))
        .rounded_full()
        .map(|button| match running {
            true => button.icon(StopResponseIcon),
            false => button.icon(IconName::ArrowUp),
        })
        .tooltip(label.clone())
        .accessibility_label(label)
}
