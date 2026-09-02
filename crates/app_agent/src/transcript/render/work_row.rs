//! Tool calls, shell output, and the rest of what the agent does between one
//! message and the next.
//!
//! A work row collapses to a title and opens to whatever the call produced, so
//! most of what it draws is only reachable once the reader asks for it.

use std::borrow::Cow;
use std::time::Instant;

use gpui::prelude::*;
use gpui::{
    AnyElement, Context, ListHorizontalSizingBehavior, ScrollHandle, SharedString, Window, div, px,
    uniform_list,
};
use gpui_component::modern_menu::ModernMenuExt as _;
use gpui_component::scroll::Scrollbar;
use gpui_component::{ActiveTheme as _, IconName};
use nmt_agent_utils::chat::Item as SessionItem;
use nmt_i18n::i18n;

use crate::settings::{AgentSettings, UI_RADIUS};
use crate::transcript::disclosure_row::{
    AGENT_CARD_BODY_PADDING_Y, AGENT_CARD_DETAIL_SIZE, AGENT_CARD_PADDING_X, AGENT_CARD_RADIUS,
    AGENT_DISCLOSURE_DETAIL_INSET, AgentCardTone, AgentDisclosureRow, agent_card,
};
use crate::transcript::render::TRANSCRIPT_LINE_HEIGHT;
use crate::transcript::reveal::{RevealKey, RevealedPart, revealed_block};
use crate::transcript::{
    TranscriptView, VirtualTranscriptState, code_transcript_format, command_execution_detail,
    command_execution_heading, command_failure_reason, fenced_code_block_as,
    should_virtualize_transcript, strip_read_gutter,
};

impl TranscriptView {
    /// One step of the work log, as a card: icon block · heading · outcome
    /// mark. A collapsed card states what the step was and how it went, and
    /// nothing else — the command it ran and the output it produced are the
    /// first thing behind the disclosure. Rows with detail (command output,
    /// reasoning text) expand on click into a bounded transcript surface with
    /// its own scroll position, drawn inside the same card so the detail stays
    /// visibly attached to the step it belongs to.
    pub(crate) fn render_work_row(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let cwd = self.cwd.clone();
        let (icon, heading, reason, status, detail) = match &self.items[index].item {
            SessionItem::CommandExecution {
                command,
                purpose,
                aggregated_output,
                status,
                exit_code,
                ..
            } => {
                // Belt and braces: a non-zero exit code is a failure even if
                // the provider reported the execution as completed.
                let state = status.as_deref().unwrap_or("inProgress");
                let failed = matches!(state, "failed" | "declined")
                    || exit_code.is_some_and(|code| code != 0);
                let state = if failed { "failed" } else { state };
                let detail = command_execution_detail(command, aggregated_output.as_deref());

                (
                    IconName::SquareTerminal,
                    command_execution_heading(purpose.as_deref()).to_string(),
                    failed
                        .then(|| command_failure_reason(aggregated_output.as_deref()))
                        .flatten(),
                    Some(state.to_string()),
                    Some(Cow::Owned(detail)),
                )
            }
            SessionItem::FileChange {
                paths,
                diff,
                status,
                ..
            } => (
                IconName::File,
                i18n("agent-transcript-edit-paths").replace("{paths}", paths),
                None,
                Some(status.as_deref().unwrap_or("inProgress").to_string()),
                diff.as_deref()
                    .filter(|diff| !diff.trim().is_empty())
                    .map(Cow::Borrowed),
            ),
            SessionItem::Other {
                kind,
                title,
                output,
                status,
                ..
            } => (
                if kind == "webSearch" {
                    IconName::Globe
                } else {
                    IconName::Settings2
                },
                if title.trim().is_empty() {
                    kind.clone()
                } else {
                    format!("{kind} {title}")
                },
                None,
                Some(status.as_deref().unwrap_or("inProgress").to_string()),
                output
                    .as_deref()
                    .filter(|output| !output.trim().is_empty())
                    .map(Cow::Borrowed),
            ),
            SessionItem::Reasoning { summary, .. } => (
                IconName::Bot,
                i18n("agent-transcript-thinking").to_string(),
                None,
                None,
                summary
                    .as_deref()
                    .filter(|text| !text.trim().is_empty())
                    .map(Cow::Borrowed),
            ),
            _ => return div().into_any_element(),
        };

        let expandable = detail.is_some();
        let expanded = expandable && self.expanded_rows.contains(&index);
        let detail_reveal = self
            .reveals
            .progress(RevealKey::Row(index), 0, Instant::now());
        let detail_part = RevealedPart::Block(RevealKey::Row(index));
        let detail_height = self.revealed_heights.get(&detail_part).copied();
        let detail_view = cx.entity().downgrade();
        let status_label = match status.as_deref() {
            Some("failed") => i18n("agent-transcript-status-failed"),
            Some("declined") => i18n("agent-transcript-status-declined"),
            Some("completed") => i18n("agent-transcript-status-completed"),
            Some("inProgress") => i18n("agent-transcript-status-in-progress"),
            Some(status) => status,
            None => i18n("agent-transcript-no-status"),
        };
        // The outcome is a mark rather than a word: it lands in the same slot
        // on every card, so a run of steps can be scanned down that column
        // instead of read. The wording stays in the row's accessible label.
        let tone = match status.as_deref() {
            Some("failed" | "declined") => AgentCardTone::Failed,
            _ => AgentCardTone::Neutral,
        };
        let status_icon = status.as_deref().map(|state| match state {
            "failed" | "declined" => (IconName::CircleX, cx.theme().danger),
            "completed" => (IconName::Check, cx.theme().success),
            _ => (IconName::Minus, cx.theme().muted_foreground),
        });

        let accessible_label = format!(
            "{}. {}{}",
            heading,
            status_label,
            if expandable {
                if self.is_disclosing(RevealKey::Row(index)) {
                    i18n("agent-transcript-accessibility-expanded")
                } else {
                    i18n("agent-transcript-accessibility-collapsed")
                }
            } else {
                ""
            }
        );
        // A failure reason shows whether or not the step is expanded, so a
        // failed row usually heads a block even while its output is hidden.
        // Otherwise the header heads a block for exactly as long as there is
        // one: it squares off with the detail's arrival and returns to a pill
        // the moment the detail has finished shrinking away.
        let heads_body = reason.is_some() || (expanded && detail_reveal > 0.0);
        let mut header = AgentDisclosureRow::new(("wl-head", index), heading)
            .type_icon(icon)
            .tone(tone)
            .heads_body(heads_body)
            .accessible_label(accessible_label);
        if let Some((icon, color)) = status_icon {
            header = header.status(icon, color);
        }
        if expandable {
            header = header.expanded(expanded).opening(detail_reveal);
        }
        let header =
            header.render(cx).when(expandable, |this| {
                this.on_click(cx.listener(move |this, _, _, cx| {
                    this.toggle_disclosure(RevealKey::Row(index), cx)
                }))
            });

        // The block under the header carries the header's own fill, so a
        // failed step reads as one tinted shape rather than as a tinted
        // heading with untinted output hanging off it.
        let card_body = heads_body.then(|| {
            div()
                .w_full()
                .bg(tone.colors(cx).background)
                .rounded_b(px(AGENT_CARD_RADIUS))
                // Why a step failed belongs on the card rather than behind the
                // disclosure: it is what the reader decides their next move from,
                // and the full transcript below it is usually a stack trace.
                .children(reason.map(|reason| {
                    div()
                        .w_full()
                        .pl(px(AGENT_DISCLOSURE_DETAIL_INSET))
                        .pr(px(AGENT_CARD_PADDING_X))
                        .pb(px(AGENT_CARD_BODY_PADDING_Y))
                        .text_size(px(AGENT_CARD_DETAIL_SIZE))
                        .text_color(cx.theme().danger.opacity(0.85))
                        .child(reason)
                }))
                .children(detail.as_deref().filter(|_| expanded).map(|detail| {
                    let code_format = code_transcript_format(&self.items[index].item, detail);
                    let virtualized = should_virtualize_transcript(code_format.is_some(), detail);

                    let body = if virtualized {
                        let (_, strip_gutter) = code_format.as_ref().expect("code format");
                        let (source, segments, widest_segment, scroll) = {
                            let state =
                                self.virtual_transcripts.entry(index).or_insert_with(|| {
                                    VirtualTranscriptState::new(detail, *strip_gutter)
                                });
                            state.sync(detail, *strip_gutter);
                            (
                                state.source.clone(),
                                state.segments.clone(),
                                state.widest_segment,
                                state.scroll.clone(),
                            )
                        };
                        let segment_count = segments.len();
                        let settings = cx.global::<AgentSettings>();
                        let transcript_font = settings.transcript_font();
                        let transcript_font_size = px(settings.transcript_font_size);
                        let line_height = transcript_font_size * TRANSCRIPT_LINE_HEIGHT;
                        let text_color = cx.theme().muted_foreground;
                        let list_source = source.clone();
                        let list_segments = segments.clone();

                        div()
                            .id(("wl-out", index))
                            .w_full()
                            .h(px(256.))
                            .relative()
                            .overflow_hidden()
                            .rounded(UI_RADIUS)
                            .bg(cx.theme().tokens.muted)
                            .font(transcript_font)
                            .text_size(transcript_font_size)
                            // The outer transcript list is behind this viewport in
                            // hit-test order. Occlusion prevents wheel input at the
                            // virtual list's limits from moving the conversation.
                            .occlude()
                            .modern_context_menu(Self::copy_menu(cx.entity().downgrade(), index))
                            .child(
                                uniform_list(
                                    SharedString::from(format!("wl-virtual-{index}")),
                                    segment_count,
                                    move |range, _, _| {
                                        range
                                            .filter_map(|segment_index| {
                                                let range =
                                                    list_segments.get(segment_index)?.clone();
                                                Some(
                                                    div()
                                                        .h(line_height)
                                                        .flex_none()
                                                        .line_height(line_height)
                                                        .whitespace_nowrap()
                                                        .text_color(text_color)
                                                        .child(list_source[range].to_string()),
                                                )
                                            })
                                            .collect::<Vec<_>>()
                                    },
                                )
                                .with_width_from_item(Some(widest_segment))
                                .with_horizontal_sizing_behavior(
                                    ListHorizontalSizingBehavior::Unconstrained,
                                )
                                .track_scroll(&scroll)
                                .size_full()
                                .p_3(),
                            )
                            .child(
                                div()
                                    .absolute()
                                    .top_0()
                                    .right_0()
                                    .bottom_0()
                                    .w(px(16.0))
                                    .child(
                                        Scrollbar::vertical(&scroll)
                                            .id(("wl-virtual-v-scrollbar", index)),
                                    ),
                            )
                            .child(
                                div()
                                    .absolute()
                                    .left_0()
                                    .right_0()
                                    .bottom_0()
                                    .h(px(16.0))
                                    .child(
                                        Scrollbar::horizontal(&scroll)
                                            .id(("wl-virtual-h-scrollbar", index)),
                                    ),
                            )
                            .into_any_element()
                    } else {
                        self.virtual_transcripts.remove(&index);

                        // Small technical transcripts retain syntax highlighting;
                        // Markdown-native tool output and reasoning retain their
                        // rich formatting. The expensive fence and gutter copy are
                        // therefore bounded by the virtualization thresholds.
                        let markdown = match code_format {
                            Some((language, strip_gutter)) => {
                                let normalized = if strip_gutter {
                                    strip_read_gutter(detail)
                                        .map(Cow::Owned)
                                        .unwrap_or(Cow::Borrowed(detail))
                                } else {
                                    Cow::Borrowed(detail)
                                };
                                fenced_code_block_as(&normalized, language.as_ref())
                            }
                            None => detail.to_owned(),
                        };
                        let detail_scroll = window
                            .use_keyed_state(("wl-scroll", index), cx, |_, _| {
                                ScrollHandle::default()
                            })
                            .read(cx)
                            .clone();

                        div()
                            .w_full()
                            .relative()
                            .child(
                                div()
                                    .id(("wl-out", index))
                                    .w_full()
                                    .max_h(px(256.))
                                    .overflow_y_scroll()
                                    .track_scroll(&detail_scroll)
                                    // The virtual conversation list handles wheel input
                                    // before child bubble listeners run. Occluding its
                                    // earlier hitbox makes this viewport the only scroll
                                    // target under the pointer, even at either limit.
                                    .occlude()
                                    .modern_context_menu(Self::copy_menu(
                                        cx.entity().downgrade(),
                                        index,
                                    ))
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(
                                        Self::markdown_view(
                                            ("wl-md", index),
                                            markdown,
                                            cwd.clone(),
                                        )
                                        .style(Self::work_detail_text_style(cx))
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
                                        Scrollbar::vertical(&detail_scroll)
                                            .id(("wl-scrollbar", index)),
                                    ),
                            )
                            .into_any_element()
                    };

                    let block = div()
                        // Expanded content takes the card's own inset on both
                        // sides. The rule above it already says the detail belongs
                        // to the header, so indenting it as well would spend a
                        // third of a narrow card on saying it twice — and command
                        // output is exactly the content that needs the width.
                        .w_full()
                        .border_t_1()
                        .border_color(cx.theme().border.opacity(0.6))
                        .px(px(AGENT_CARD_PADDING_X))
                        .py(px(AGENT_CARD_BODY_PADDING_Y))
                        .child(body);

                    revealed_block(
                        block,
                        detail_part,
                        detail_reveal,
                        detail_height,
                        px(0.),
                        detail_view,
                    )
                }))
        });

        agent_card()
            .id(("entry", index))
            .modern_context_menu(Self::copy_menu(cx.entity().downgrade(), index))
            .child(header)
            .children(card_body)
            .into_any_element()
    }
}
