//! The single-entry rows of a transcript that carry no disclosure: the live
//! working line, assistant replies, errors, and replies that asked questions.

use std::time::Instant;

use chrono::{DateTime, Local};
use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, Div, FontWeight, SharedString, WeakEntity, div, px, relative,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::modern_menu::ModernMenuExt as _;
use gpui_component::shimmer::ShimmerText;
use gpui_component::spinner::Spinner;
use gpui_component::{ActiveTheme as _, IconName, Sizable as _, h_flex, v_flex};
use nmt_agent::chat::Question;
use rust_i18n::t;

use crate::agent_tab::AgentPane;
use crate::agent_tab::settings::UI_RADIUS;
use crate::agent_tab::transcript::disclosure_row::{
    AGENT_CARD_DETAIL_SIZE, AGENT_CARD_GAP, AGENT_CARD_ICON_BLOCK, AGENT_CARD_PADDING_X,
};
use crate::agent_tab::transcript::render::WorkingIndicator;
use crate::agent_tab::transcript::render::menus::copy_entry_menu;
use crate::agent_tab::transcript::render::text_style::{markdown_view, transcript_text_style};
use crate::agent_tab::transcript::{TranscriptView, working_label};

/// The live line under a running turn, which began at `started`. While the
/// conversation is being compacted it says that instead of the waiting swell.
pub(crate) fn working_row(
    started: Instant,
    output_tokens: Option<u64>,
    detail: Option<&str>,
    compacting: bool,
    cx: &App,
) -> AnyElement {
    let label = working_label(started, output_tokens, detail);

    if compacting {
        let accent = cx.theme().info;

        return h_flex()
            .w_full()
            .gap(px(AGENT_CARD_GAP))
            .items_center()
            .px(px(AGENT_CARD_PADDING_X))
            .child(
                div()
                    .size(px(AGENT_CARD_ICON_BLOCK))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        Spinner::new()
                            .icon(IconName::LoaderCircle)
                            .with_size(px(12.))
                            .color(accent),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(accent)
                            .child(t!("agent-transcript-compacting")),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(label),
                    ),
            )
            .into_any_element();
    }

    // The dots stand in the slot a card gives its type icon, so the label
    // starts on the column a tool call's title starts on and the live line
    // reads as the next step of the work above it rather than as a stray
    // line under it. A ring turning in that slot reads as one more step
    // with an icon; a travelling swell reads as the pane waiting.
    h_flex()
        .w_full()
        .gap(px(AGENT_CARD_GAP))
        .items_center()
        .px(px(AGENT_CARD_PADDING_X))
        .child(
            div()
                .size(px(AGENT_CARD_ICON_BLOCK))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .child(WorkingIndicator::new(cx.theme().warning)),
        )
        .child(
            div()
                .text_size(px(AGENT_CARD_DETAIL_SIZE))
                .text_color(cx.theme().muted_foreground)
                .child(
                    // The label text changes every second, so it cannot
                    // serve as the animation identity; a fixed id keeps
                    // one animation state alive across those rewrites.
                    //
                    // The band lifts the muted label to full foreground
                    // contrast. The component's theme-derived default
                    // mixes the text toward the background on light
                    // themes, which fades the band into the page instead,
                    // and its default peak leaves the muted label only
                    // slightly lifted at the twelve-pixel detail size.
                    ShimmerText::new(label)
                        .id("agent-working-label")
                        .highlight_color(cx.theme().foreground)
                        .peak_opacity(0.9),
                ),
        )
        .into_any_element()
}

/// An assistant reply at entry `index`. `author` names the agent on a
/// conversation that mixes several, `cwd` resolves the reply's links, and
/// `at` is when it arrived.
pub(crate) fn agent_reply_row(
    index: usize,
    text: String,
    cwd: Option<String>,
    author: Option<SharedString>,
    at: Option<i64>,
    cx: &mut Context<TranscriptView>,
) -> AnyElement {
    h_flex()
        .id(("entry", index))
        .group("entry")
        .relative()
        .w_full()
        .items_end()
        .modern_context_menu(copy_entry_menu(cx.entity().downgrade(), index))
        .child(
            v_flex()
                .debug_selector(move || format!("transcript-agent-{index}"))
                .flex_1()
                .min_w_0()
                .px_1()
                .when_some(author, |view, author| {
                    view.child(
                        div()
                            .debug_selector(move || format!("transcript-author-{index}"))
                            .mb_2()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(cx.theme().muted_foreground)
                            .child(author),
                    )
                })
                .child(
                    markdown_view(("agent-md", index), text, cwd)
                        .style(transcript_text_style(cx))
                        .selectable(true),
                ),
        )
        .child(
            // A stamp in the flow would reserve its width on every row,
            // ending assistant output short of the pane by a strip that is
            // blank whenever the pointer is elsewhere. Out of the flow it
            // costs nothing until it appears, and the tinted chip keeps it
            // legible where it lands over the last line.
            hover_stamp(at, cx)
                .absolute()
                .right_1()
                .bottom_0()
                .px_1()
                .rounded(UI_RADIUS)
                .bg(cx.theme().muted),
        )
        .into_any_element()
}

/// An error the harness reported, at entry `index`.
pub(crate) fn error_row(
    index: usize,
    text: String,
    cx: &mut Context<TranscriptView>,
) -> AnyElement {
    h_flex()
        .id(("entry", index))
        .w_full()
        .modern_context_menu(copy_entry_menu(cx.entity().downgrade(), index))
        .child(
            div()
                .max_w(relative(0.9))
                .px_3()
                .py_2()
                .rounded(UI_RADIUS)
                .bg(cx.theme().danger.opacity(0.15))
                .text_color(cx.theme().danger)
                .text_sm()
                .child(text),
        )
        .into_any_element()
}

/// Hover-revealed timestamp of an entry that arrived at `at`; the row
/// declares `.group("entry")`.
pub(crate) fn hover_stamp(at: Option<i64>, cx: &App) -> Div {
    div()
        .flex_none()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .invisible()
        .group_hover("entry", |this| this.visible())
        .child(
            at.and_then(|at| DateTime::from_timestamp(at, 0))
                .map(|at| at.with_timezone(&Local).format("%H:%M").to_string())
                .unwrap_or_default(),
        )
}

/// A reply that asked the user questions: the `reply` row, and a control
/// that reopens `questions` in the `owner` pane's question card.
pub(crate) fn question_message(
    index: usize,
    id: String,
    questions: Vec<Question>,
    reply: AnyElement,
    owner: Option<WeakEntity<AgentPane>>,
    cx: &App,
) -> AnyElement {
    v_flex()
        .w_full()
        .gap_2()
        .p_3()
        .rounded(UI_RADIUS)
        .border_1()
        .border_color(cx.theme().border)
        .child(reply)
        .child(div().children(owner.map(|owner| {
            Button::new(("message-questions", index))
                .ghost()
                .small()
                .label(t!("agent-question-open"))
                .on_click(move |_, _, cx| {
                    let _ = owner.update(cx, |pane, cx| {
                        pane.open_message_questions(&id, questions.clone(), cx)
                    });
                })
        })))
        .into_any_element()
}
