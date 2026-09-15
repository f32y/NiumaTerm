use gpui::Entity;
use gpui::prelude::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::dialog::{DIALOG_BUTTON_MIN_WIDTH, Dialog, DialogClose, DialogFooter};
use gpui_component::{ActiveTheme as _, WindowExt as _, v_flex};
use rust_i18n::t;

use crate::agent_tab::AgentPane;

/// The dialog asking before a message pays for a cold prompt cache. `idle`
/// says how long the conversation has been sitting; sending goes ahead through
/// `pane`, and cancelling leaves the message in the composer.
pub(crate) fn cache_expiry_dialog(dialog: Dialog, pane: &Entity<AgentPane>, idle: &str) -> Dialog {
    let pane = pane.clone();
    let idle = idle.to_string();

    dialog
        .title(t!("agent-cache-warning-title"))
        .overlay_closable(false)
        .content(move |content, _, cx| {
            content.child(
                v_flex()
                    .gap_1()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(idle.clone())
                    .child(t!("agent-cache-warning-message")),
            )
        })
        .footer(
            DialogFooter::new()
                .child(
                    Button::new("agent-cache-warning-send")
                        .min_w(DIALOG_BUTTON_MIN_WIDTH)
                        .label(t!("agent-cache-warning-send"))
                        .on_click(move |_, window, cx| {
                            window.close_dialog(cx);

                            pane.update(cx, |pane, cx| pane.send_user_message_now(window, cx));
                        }),
                )
                .child(
                    DialogClose::new().child(
                        Button::new("agent-cache-warning-cancel")
                            .min_w(DIALOG_BUTTON_MIN_WIDTH)
                            .primary()
                            .label(t!("agent-cache-warning-cancel")),
                    ),
                ),
        )
}
