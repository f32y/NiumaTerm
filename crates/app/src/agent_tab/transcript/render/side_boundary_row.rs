//! The row that opens a side conversation, and the injected context it
//! discloses.

use std::time::Instant;

use gpui::prelude::*;
use gpui::{AnyElement, Context, ScrollHandle, SharedString, Window, div, px};
use gpui_component::modern_menu::ModernMenuExt as _;
use gpui_component::{ActiveTheme as _, IconName, v_flex};
use rust_i18n::t;

use crate::agent_tab::settings::UI_RADIUS;
use crate::agent_tab::transcript::TranscriptView;
use crate::agent_tab::transcript::disclosure_row::{
    AGENT_CARD_BODY_PADDING_Y, AGENT_CARD_PADDING_X, AgentDisclosureRow, agent_card,
};
use crate::agent_tab::transcript::render::bounded_scroll;
use crate::agent_tab::transcript::render::menus::copy_entry_menu;
use crate::agent_tab::transcript::render::text_style::markdown_view;
use crate::agent_tab::transcript::reveal::{Disclosures, RevealKey, RevealedPart, revealed_block};

/// The boundary a side conversation starts from. Drawn as a divider like a
/// compaction boundary, because it is the same kind of break: what lies
/// before it is inherited reference, not the conversation below. Expanding
/// shows the developer instructions and the message the fork was given,
/// which steer every answer and were never typed by the user.
pub(crate) fn render_side_boundary_row(
    disclosures: &Disclosures,
    index: usize,
    instructions: String,
    message: String,
    window: &mut Window,
    cx: &mut Context<TranscriptView>,
) -> AnyElement {
    let label = t!("agent-transcript-side-boundary");
    let preview = t!("agent-transcript-side-boundary-preview");
    let expanded = disclosures.row_expanded(index);
    let accent = cx.theme().info;

    let header = AgentDisclosureRow::new(("side-boundary-head", index), label.as_ref())
        .type_icon(IconName::Info)
        .preview(preview.to_string())
        .accent(accent)
        .expanded(expanded)
        .opening(disclosures.progress(RevealKey::Row(index), Instant::now()))
        .accessible_label(format!(
            "{label}. {preview}. {}",
            if disclosures.is_disclosing(RevealKey::Row(index)) {
                t!("agent-transcript-expanded")
            } else {
                t!("agent-transcript-collapsed")
            }
        ))
        .render(cx)
        .on_click(
            cx.listener(move |this, _, _, cx| this.toggle_disclosure(RevealKey::Row(index), cx)),
        );

    v_flex()
        .id(("entry", index))
        .w_full()
        .gap_1()
        .modern_context_menu(copy_entry_menu(cx.entity().downgrade(), index))
        .child(agent_card().child(header).children(expanded.then(|| {
            render_side_boundary_detail(disclosures, index, instructions, message, window, cx)
        })))
        // The rule sits below the heading: it closes off the inherited
        // history the boundary marks, and the side conversation starts under
        // it.
        .child(div().w_full().h(px(1.)).bg(accent.opacity(0.35)))
        .into_any_element()
}

/// Expanded body: each injected text under its own heading, in one bounded
/// scroll surface, since together they run to a few kilobytes.
fn render_side_boundary_detail(
    disclosures: &Disclosures,
    index: usize,
    instructions: String,
    message: String,
    window: &mut Window,
    cx: &mut Context<TranscriptView>,
) -> AnyElement {
    let scroll = window
        .use_keyed_state(("side-boundary-scroll", index), cx, |_, _| {
            ScrollHandle::default()
        })
        .read(cx)
        .clone();

    let section = |id: &'static str, heading: SharedString, text: String| {
        v_flex()
            .w_full()
            .gap_1()
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground.opacity(0.75))
                    .child(heading),
            )
            .child(markdown_view((id, index), text, None).selectable(true))
    };

    let block = v_flex()
        .w_full()
        .border_t_1()
        .border_color(cx.theme().border.opacity(0.6))
        .px(px(AGENT_CARD_PADDING_X))
        .py(px(AGENT_CARD_BODY_PADDING_Y))
        .child(bounded_scroll(
            &scroll,
            ("side-boundary-scrollbar", index),
            v_flex()
                .id(("side-boundary-text", index))
                .w_full()
                .max_h(px(320.))
                .modern_context_menu(copy_entry_menu(cx.entity().downgrade(), index))
                .px_3()
                .py_2()
                .gap_3()
                .rounded(UI_RADIUS)
                .bg(cx.theme().tokens.muted)
                .text_color(cx.theme().muted_foreground)
                .child(section(
                    "side-boundary-instructions",
                    t!("agent-transcript-side-instructions").into_owned().into(),
                    instructions,
                ))
                .child(section(
                    "side-boundary-message",
                    t!("agent-transcript-side-message").into_owned().into(),
                    message,
                )),
        ));

    let part = RevealedPart::Block(RevealKey::Row(index));

    revealed_block(
        block,
        part,
        disclosures.progress(RevealKey::Row(index), Instant::now()),
        disclosures.height(part),
        px(0.),
        cx.entity().downgrade(),
    )
    .into_any_element()
}
