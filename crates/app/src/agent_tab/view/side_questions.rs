use gpui::prelude::*;
use gpui::{AnyElement, ClipboardItem, Context, FontWeight, div, px};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{ActiveTheme as _, IconName, Sizable as _, h_flex, v_flex};
use nmt_agent::session::side::{SideAnswer, SideExchange};
use rust_i18n::t;

use crate::agent_tab::AgentPane;
use crate::agent_tab::settings::UI_RADIUS;

/// The side questions asked so far and their answers, oldest first, above the
/// composer. The card is separate from the transcript because none of this is
/// part of the conversation: closing it drops every exchange.
pub(crate) fn side_questions_card(
    exchanges: &[SideExchange],
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
            h_flex()
                .w_full()
                .gap_2()
                .items_center()
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .text_xs()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(cx.theme().muted_foreground)
                                .child(t!("agent-side-title")),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(t!("agent-side-hint")),
                        ),
                )
                .child(
                    Button::new("side-questions-close")
                        .ghost()
                        .xsmall()
                        .icon(IconName::Close)
                        .tooltip(t!("agent-side-close"))
                        .accessibility_label(t!("agent-side-close"))
                        .on_click(cx.listener(|this, _, _, cx| this.close_side_questions(cx))),
                ),
        )
        .child(
            // Answers can run long, and the composer below must stay
            // reachable, so the exchanges scroll inside a ceiling.
            v_flex()
                .id("side-questions-exchanges")
                .max_h(px(256.))
                .overflow_y_scroll()
                .gap_2()
                .children(
                    exchanges
                        .iter()
                        .enumerate()
                        .map(|(index, exchange)| side_exchange(index, exchange, cx)),
                ),
        )
        .into_any_element()
}

fn side_exchange(index: usize, exchange: &SideExchange, cx: &mut Context<AgentPane>) -> AnyElement {
    let answer = match &exchange.answer {
        SideAnswer::Pending(_) => div()
            .text_color(cx.theme().muted_foreground)
            .child(t!("agent-side-answering")),
        SideAnswer::Answered(text) => div().child(text.clone()),
        SideAnswer::Failed(message) => div().text_color(cx.theme().danger).child(message.clone()),
    };

    let actions = match &exchange.answer {
        SideAnswer::Answered(text) => {
            let copy_text = text.clone();

            Some(
                h_flex()
                    .justify_end()
                    .gap_1()
                    .child(
                        Button::new(("side-answer-copy", index))
                            .ghost()
                            .xsmall()
                            .label(t!("agent-side-copy"))
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(copy_text.clone()));
                            }),
                    )
                    .child(
                        Button::new(("side-answer-insert", index))
                            .ghost()
                            .xsmall()
                            .label(t!("agent-side-insert"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.insert_side_answer(index, window, cx)
                            })),
                    ),
            )
        }
        SideAnswer::Pending(_) | SideAnswer::Failed(_) => None,
    };

    v_flex()
        .gap_1()
        .px_3()
        .py_2()
        .rounded(UI_RADIUS)
        .border_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().background.opacity(0.7))
        .text_sm()
        .child(
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .child(exchange.question.clone()),
        )
        .child(answer)
        .children(actions)
        .into_any_element()
}
