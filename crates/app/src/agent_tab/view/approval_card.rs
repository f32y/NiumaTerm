use gpui::prelude::*;
use gpui::{AnyElement, Context, FontWeight, div, px};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{ActiveTheme as _, h_flex, v_flex};
use rust_i18n::t;

use crate::agent_tab::AgentPane;
use crate::agent_tab::settings::UI_RADIUS;

/// The card asking whether to allow the pending tool call `approval`
/// describes. `session_scoped` offers the grant for the rest of the
/// session, which only a harness able to honor it does.
pub(crate) fn approval_card(
    approval: String,
    session_scoped: bool,
    cx: &mut Context<AgentPane>,
) -> AnyElement {
    v_flex()
        .w_full()
        .px_4()
        .py_3()
        .gap_2()
        .border_b_1()
        .border_color(cx.theme().border.opacity(0.65))
        .bg(cx.theme().muted.opacity(0.2))
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(cx.theme().muted_foreground)
                .child(t!("agent-approval-pending")),
        )
        .child(
            div()
                // The description carries whatever the request holds:
                // a whole plan for ExitPlanMode, a full command line
                // for Bash. Without a ceiling the card grows past the
                // pane and the decision buttons below it are clipped
                // away, leaving the turn unanswerable.
                .id("approval-description")
                .max_h(px(256.))
                .overflow_y_scroll()
                .px_3()
                .py_2()
                .rounded(UI_RADIUS)
                .border_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().background.opacity(0.7))
                .text_sm()
                .child(approval),
        )
        .child(
            h_flex()
                .justify_end()
                .gap_2()
                .child(
                    Button::new("approval-cancel")
                        .ghost()
                        .label(t!("agent-approval-cancel-turn"))
                        .on_click(
                            cx.listener(|this, _, _, cx| this.respond_approval("cancel", cx)),
                        ),
                )
                .child(
                    Button::new("approval-decline")
                        .outline()
                        .label(t!("agent-approval-decline"))
                        .on_click(
                            cx.listener(|this, _, _, cx| this.respond_approval("decline", cx)),
                        ),
                )
                // Offered only where it means something. A harness that
                // can answer just this one call would quietly turn a
                // session-wide grant into a single-use one.
                .when(session_scoped, |this| {
                    this.child(
                        Button::new("approval-session")
                            .outline()
                            .label(t!("agent-approval-allow-session"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.respond_approval("acceptForSession", cx)
                            })),
                    )
                })
                .child(
                    Button::new("approval-accept")
                        .primary()
                        .label(t!("agent-approval-approve-once"))
                        .on_click(
                            cx.listener(|this, _, _, cx| this.respond_approval("accept", cx)),
                        ),
                ),
        )
        .into_any_element()
}
