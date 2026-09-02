mod compaction_row;
mod text_style;
mod turn_rows;
mod user_row;
mod work_row;

use std::time::Instant;

use gpui::prelude::*;
use gpui::{AnyElement, Context, Window, div, px, relative};
use gpui_component::modern_menu::ModernMenuExt as _;
use gpui_component::spinner::Spinner;
use gpui_component::{ActiveTheme as _, IconName, Sizable as _, h_flex};
use nmt_agent_utils::chat::Item as SessionItem;
use nmt_i18n::i18n;

use crate::settings::UI_RADIUS;
use crate::transcript::disclosure_row::{
    AGENT_CARD_DETAIL_SIZE, AGENT_CARD_GAP, AGENT_CARD_ICON_BLOCK, AGENT_CARD_PADDING_X,
};
use crate::transcript::reveal::{RevealedPart, revealed_block};
use crate::transcript::rows::{RowGap, TranscriptRow, is_run_row};
use crate::transcript::working_indicator::WorkingIndicator;
use crate::transcript::{RowSpec, TranscriptView, is_work_row, working_label};

/// Edge of a transcript thumbnail, matching the composer strip so an image
/// does not change size when the message it belongs to is sent.
const TRANSCRIPT_THUMBNAIL: f32 = 56.0;

/// Share of the pane the conversation column takes, and the margin on each
/// side that leaves. Expressed as a share rather than as a fixed measure
/// because a fixed one reads as a narrow strip down the middle of a wide
/// display.
pub(crate) const TRANSCRIPT_COLUMN_FRACTION: f32 = 0.8;
pub(crate) fn transcript_column_margin() -> f32 {
    (1.0 - TRANSCRIPT_COLUMN_FRACTION) / 2.0
}
/// The measure assistant prose wraps at, inside that column: 880px at the
/// default root size, which is around 90 latin characters or 45 CJK ones a
/// line. Held in rems so it tracks the root size the rest of the UI scales
/// with rather than pinning a physical width.
const PROSE_MEASURE_REMS: f32 = 55.0;
/// Three ranks of space, which is what makes a turn read as message / work /
/// message rather than as one undifferentiated stack. The widest marks where
/// one exchange ends; the middle one holds a turn's work off the prose it is
/// interleaved with, close enough that the two still read as one answer; the
/// tightest keeps the steps of a single run together.
const TRANSCRIPT_GROUP_GAP: f32 = 24.0;
const TRANSCRIPT_WORK_TEXT_GAP: f32 = 12.0;
const TRANSCRIPT_STEP_GAP: f32 = 8.0;
/// The rule down the left of a run of work rows. The steps carry no border of
/// their own, so this is what marks where a run starts and ends and keeps its
/// rows reading as one block. It holds the rows off nothing: a gap after it
/// would indent the run's labels away from the column the conversation is
/// read in, and the rule already separates them from it.
const TRANSCRIPT_RUN_RULE: f32 = 2.0;
/// Where the conversation's own text starts inside the reading column, which
/// every prose row and status line sets on itself. The run rule stands on
/// that edge rather than left of it, so a run reads as part of the column
/// instead of hanging off it.
const TRANSCRIPT_TEXT_INSET: f32 = 4.0;
/// Leading for transcript text, as a multiple of the font size. Conversation
/// prose is read in paragraphs rather than scanned line by line the way
/// terminal output is, so it is set looser than the chrome around it.
pub(super) const TRANSCRIPT_LINE_HEIGHT: f32 = 1.6;

impl TranscriptView {
    /// Build the element for one visible row. Row indices come from the list
    /// element during layout/paint, resolved through the spec snapshot taken
    /// in the current render pass.
    pub(crate) fn render_row(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(TranscriptRow { spec, gap }) = self.rows.get(ix).cloned() else {
            return div().into_any_element();
        };

        // The list lays each row out on its own, so a run's grouping rule is
        // drawn per row rather than around the run. A segment has to carry
        // the gap below it or consecutive segments meet with a break between
        // them, and a run continues past a boundary exactly when that
        // boundary is step-ranked. The wider ranks all end the run, so their
        // space belongs below the rule rather than inside it.
        let in_run = is_run_row(&spec);
        let rule_carries_gap = in_run && gap == RowGap::Step;
        let gap = match gap {
            RowGap::Step => TRANSCRIPT_STEP_GAP,
            RowGap::Work => TRANSCRIPT_WORK_TEXT_GAP,
            RowGap::Group => TRANSCRIPT_GROUP_GAP,
        };

        // A run's steps open and shut as list rows of their own, so each one
        // ramps its own height and needs a height of its own to ramp towards.
        let step = match &spec {
            RowSpec::Work { index, .. } => Some(RevealedPart::Step(*index)),
            _ => None,
        };

        let row = match spec {
            RowSpec::Entry { index, .. } => self.render_entry_row(index, window, cx),
            RowSpec::Work { index, .. } => self.render_work_row(index, window, cx),
            RowSpec::TurnFold {
                turn,
                row_count,
                folded,
            } => self.render_turn_fold(turn, row_count, folded, cx),
            RowSpec::TurnSummary {
                seconds,
                output_tokens,
            } => self.render_turn_summary(seconds, output_tokens, cx),
            RowSpec::Interrupted { output_tokens, .. } => {
                self.render_interrupted_row(output_tokens, cx)
            }
            RowSpec::RunToggle {
                run_start,
                tool_count,
                expanded,
            } => self.render_run_toggle(run_start, tool_count, expanded, cx),
            RowSpec::Working { compacting } => self.render_working_row(compacting, cx),
        };

        // Each row is laid out on its own by the virtual list, so the reading
        // column has to be re-established per row rather than once around the
        // conversation. The margin does that here rather than a centered inner
        // box: an inner box is one more level for a width to resolve through,
        // and a row whose width goes indefinite wraps its text at the minimum
        // — one glyph per line for CJK prose.
        let body = div()
            .w_full()
            .when(in_run, |this| {
                this.border_l(px(TRANSCRIPT_RUN_RULE))
                    .border_color(cx.theme().border)
            })
            .when(rule_carries_gap, |this| this.pb(px(gap)))
            .child(row);

        let row = div()
            .w_full()
            .px(relative(transcript_column_margin()))
            .when(!rule_carries_gap, |this| this.pb(px(gap)))
            // A border is drawn at the element's own leading edge, outside any
            // padding it carries, so the inset that puts the rule on the text
            // column has to come from a level above it. Only a run needs one,
            // and only a run pays for it.
            .map(|this| match in_run {
                true => this.child(div().w_full().pl(px(TRANSCRIPT_TEXT_INSET)).child(body)),
                false => this.child(body),
            });

        // A row a run toggle spliced in grows and shrinks rather than
        // appearing and vanishing, so the conversation below the run travels
        // with it the whole way instead of catching up in one jump at the end.
        // The ramp wraps the row entire — its slice of the grouping rule and
        // the space it owes the row below it — because a rule drawn down to a
        // step of no height, or a gap left where a step used to be, is the
        // part that would still jump.
        match (step, self.revealed_by(ix)) {
            (Some(part), Some((key, ordinal))) => revealed_block(
                row,
                part,
                self.reveals.progress(key, ordinal, Instant::now()),
                self.revealed_heights.get(&part).copied(),
                // The space under a run's last step is the space its toggle
                // takes over the moment the run leaves: both boundaries are
                // read off the same pair of rows, so both are worth the same
                // rank. Holding that much back makes the two changes cancel.
                // Every step above the last owes nothing, because the step
                // rhythm it carries is the one the toggle already sits on.
                px(gap - TRANSCRIPT_STEP_GAP),
                cx.entity().downgrade(),
            )
            .into_any_element(),
            _ => row.into_any_element(),
        }
    }

    /// The live progress line. While the backend is compacting it names that
    /// explicitly and spins: compaction produces no streamed output, so a bare
    /// seconds counter would read as a hung turn for as long as a minute.
    pub(crate) fn render_working_row(
        &self,
        compacting: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(started) = self.working_started else {
            return div().into_any_element();
        };

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
                                .child(i18n("agent-transcript-compacting")),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(working_label(
                                    started,
                                    self.working_output_tokens,
                                    self.working_detail.as_deref(),
                                )),
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
                    .child(working_label(
                        started,
                        self.working_output_tokens,
                        self.working_detail.as_deref(),
                    )),
            )
            .into_any_element()
    }

    pub(crate) fn render_entry_row(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let entry = &self.items[index];

        match &entry.item {
            SessionItem::UserMessage { text: Some(text) } => self.render_user_row(index, text, cx),
            SessionItem::AgentMessage {
                text: Some(text), ..
            } => self.render_agent_row(index, text.clone(), cx),
            SessionItem::Error { text } => self.render_error_row(index, text.clone(), cx),
            SessionItem::Compaction { detail, .. } => {
                let detail = detail.clone();

                self.render_compaction_row(index, detail, window, cx)
            }
            item if is_work_row(item) => self.render_work_row(index, window, cx),
            _ => div().into_any_element(),
        }
    }

    pub(crate) fn render_agent_row(
        &self,
        index: usize,
        text: String,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        h_flex()
            .id(("entry", index))
            .group("entry")
            .relative()
            .w_full()
            .items_end()
            .modern_context_menu(Self::copy_menu(cx.entity().downgrade(), index))
            .child(
                div().flex_1().min_w_0().px_1().child(
                    Self::markdown_view(("agent-md", index), text, self.cwd.clone())
                        .style(Self::agent_text_style(cx))
                        .selectable(true),
                ),
            )
            .child(
                // A stamp in the flow would reserve its width on every row,
                // ending assistant output short of the pane by a strip that is
                // blank whenever the pointer is elsewhere. Out of the flow it
                // costs nothing until it appears, and the tinted chip keeps it
                // legible where it lands over the last line.
                self.hover_stamp(index, cx)
                    .absolute()
                    .right_1()
                    .bottom_0()
                    .px_1()
                    .rounded(UI_RADIUS)
                    .bg(cx.theme().muted),
            )
            .into_any_element()
    }

    pub(crate) fn render_error_row(
        &self,
        index: usize,
        text: String,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        h_flex()
            .id(("entry", index))
            .w_full()
            .modern_context_menu(Self::copy_menu(cx.entity().downgrade(), index))
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
}
