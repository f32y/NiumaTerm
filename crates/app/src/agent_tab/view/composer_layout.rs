use gpui::prelude::*;
use gpui::{AnyElement, App, Div, ElementId, SharedString, div, px, relative};
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

/// The place a panel takes above the composer card: centred on the column,
/// with its lower edge pulled behind whatever is drawn next by the same
/// amount the panel pads its bottom, so its rounded top corners show and its
/// join with the card does not.
pub(crate) fn composer_panel_slot(panel: Div) -> Div {
    div()
        .w_full()
        .flex()
        .justify_center()
        .mb(px(-COMPOSER_PANEL_TUCK))
        .child(panel)
}

/// The strip above the input that carries what the composer has to say
/// beside the message: command acknowledgements, errors, and the prompts
/// queued behind a running turn. Drawn as its own panel rather than inside
/// the card, so the card stays the message and its controls, and the strip
/// comes and goes without moving the input. `None` when there is nothing to
/// say, so the card sits flush against what is above it.
pub(crate) fn composer_notice_panel(notices: Vec<AnyElement>, cx: &App) -> Option<AnyElement> {
    if notices.is_empty() {
        return None;
    }

    Some(
        composer_panel_slot(
            composer_panel(cx)
                .debug_selector(|| "agent-notice-panel".into())
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .children(notices),
        )
        .into_any_element(),
    )
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
