//! The row that marks where the conversation was summarized, and the detail it
//! opens into.
//!
//! Only a harness that reports what it summarized can offer the detail; the
//! others draw the marker alone.

use std::time::Instant;

use gpui::prelude::*;
use gpui::{AnyElement, Context, ScrollHandle, Window, div, px};
use gpui_component::modern_menu::ModernMenuExt as _;
use gpui_component::scroll::Scrollbar;
use gpui_component::{ActiveTheme as _, IconName, h_flex, v_flex};
use nmt_agent_utils::chat::Compaction;
use nmt_i18n::i18n;

use crate::settings::UI_RADIUS;
use crate::transcript::disclosure_row::{
    AGENT_CARD_BODY_PADDING_Y, AGENT_CARD_PADDING_X, AgentDisclosureRow, agent_card,
};
use crate::transcript::reveal::{RevealKey, RevealedPart, revealed_block};
use crate::transcript::{
    TranscriptView, compact_token_count, compaction_accounting, compaction_label,
    compaction_row_is_expandable, compaction_trigger_label,
};

impl TranscriptView {
    /// A context-compaction boundary. Rendered as a divider rather than a card
    /// because it is a structural break in the conversation: the rows above it
    /// are no longer what the model sees. Expanding reveals the accounting and
    /// the summary the thread continued from when the provider exposes it.
    pub(crate) fn render_compaction_row(
        &self,
        index: usize,
        detail: Compaction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let label = compaction_label(&detail);
        let preview = compaction_accounting(&detail).join(" · ");
        let expandable = compaction_row_is_expandable(self.kind);
        let expanded = expandable && self.expanded_rows.contains(&index);
        let accent = cx.theme().info;

        let mut header_row = AgentDisclosureRow::new(("compaction-head", index), label)
            .type_icon(IconName::Minimize)
            .preview(preview.clone())
            .accent(accent);
        if expandable {
            header_row = header_row.expanded(expanded).opening(self.reveals.progress(
                RevealKey::Row(index),
                0,
                Instant::now(),
            ));
        }
        let accessible_label = if expandable {
            format!(
                "{label}. {preview}. {}",
                if self.is_disclosing(RevealKey::Row(index)) {
                    i18n("agent-transcript-expanded")
                } else {
                    i18n("agent-transcript-collapsed")
                }
            )
        } else {
            format!("{label}. {preview}")
        };
        let mut header = header_row.accessible_label(accessible_label).render(cx);
        if expandable {
            header =
                header.on_click(cx.listener(move |this, _, _, cx| {
                    this.toggle_disclosure(RevealKey::Row(index), cx)
                }));
        }

        v_flex()
            .id(("entry", index))
            .w_full()
            .gap_1()
            .modern_context_menu(Self::copy_menu(cx.entity().downgrade(), index))
            // The rule sits above the heading: it closes off the conversation
            // that the summary below replaced.
            .child(div().w_full().h(px(1.)).bg(accent.opacity(0.35)))
            .child(agent_card().child(header).children(
                expanded.then(|| self.render_compaction_detail(index, &detail, window, cx)),
            ))
            .into_any_element()
    }

    /// Expanded compaction body: the accounting as labelled rows, the manual
    /// instructions when the user gave any, and the summary itself in a bounded
    /// scroll surface (summaries routinely run to several kilobytes).
    pub(crate) fn render_compaction_detail(
        &self,
        index: usize,
        detail: &Compaction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let accounting_rows = [
            (
                i18n("agent-transcript-before"),
                detail.pre_tokens.map(compact_token_count),
            ),
            (
                i18n("agent-transcript-after"),
                detail.post_tokens.map(compact_token_count),
            ),
            (
                i18n("agent-transcript-messages-summarized"),
                detail.messages_summarized.map(|count| count.to_string()),
            ),
            (
                i18n("agent-transcript-trigger"),
                detail
                    .trigger
                    .map(|trigger| compaction_trigger_label(trigger).to_string()),
            ),
            (
                i18n("agent-transcript-instructions"),
                detail.user_context.clone(),
            ),
        ];
        let summary_scroll = window
            .use_keyed_state(("compaction-scroll", index), cx, |_, _| {
                ScrollHandle::default()
            })
            .read(cx)
            .clone();

        let block = v_flex()
            .w_full()
            .border_t_1()
            .border_color(cx.theme().border.opacity(0.6))
            .px(px(AGENT_CARD_PADDING_X))
            .py(px(AGENT_CARD_BODY_PADDING_Y))
            .gap_1()
            .text_xs()
            .children(accounting_rows.into_iter().filter_map(|(name, value)| {
                let value = value?;

                Some(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .items_start()
                        .child(
                            div()
                                .flex_none()
                                .w(px(148.))
                                .text_color(cx.theme().muted_foreground.opacity(0.75))
                                .child(name),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_color(cx.theme().muted_foreground)
                                .child(value),
                        ),
                )
            }))
            .children(detail.summary.clone().map(|summary| {
                div()
                    .w_full()
                    .mt_1()
                    .relative()
                    .child(
                        div()
                            .id(("compaction-summary", index))
                            .w_full()
                            .max_h(px(320.))
                            .overflow_y_scroll()
                            .track_scroll(&summary_scroll)
                            // The virtual conversation list handles wheel input
                            // before child listeners run. Occluding its earlier
                            // hitbox makes this the only scroll target under the
                            // pointer, even at either limit.
                            .occlude()
                            .modern_context_menu(Self::copy_menu(cx.entity().downgrade(), index))
                            .px_3()
                            .py_2()
                            .rounded(UI_RADIUS)
                            .bg(cx.theme().tokens.muted)
                            .text_color(cx.theme().muted_foreground)
                            .child(
                                Self::markdown_view(
                                    ("compaction-md", index),
                                    summary,
                                    self.cwd.clone(),
                                )
                                .selectable(true),
                            ),
                    )
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .right_0()
                            .bottom_0()
                            .w(px(16.0))
                            .child(
                                Scrollbar::vertical(&summary_scroll)
                                    .id(("compaction-scrollbar", index)),
                            ),
                    )
            }));

        let part = RevealedPart::Block(RevealKey::Row(index));

        revealed_block(
            block,
            part,
            self.reveals
                .progress(RevealKey::Row(index), 0, Instant::now()),
            self.revealed_heights.get(&part).copied(),
            px(0.),
            cx.entity().downgrade(),
        )
        .into_any_element()
    }
}
