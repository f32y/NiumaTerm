mod compaction_row;
pub(super) mod image_preview;
pub(super) mod text_style;

use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{
    Animation, AnimationExt as _, AnyElement, App, Context, Div, ElementId, FontWeight, Hsla,
    Pixels, RenderOnce, Window, div, ease_in_out, px, relative, rems,
};
use gpui_component::modern_menu::ModernMenuExt as _;
use gpui_component::shimmer::ShimmerText;
use gpui_component::spinner::Spinner;
use gpui_component::{ActiveTheme as _, IconName, Sizable as _, h_flex, v_flex};
use nmt_agent::chat::Item as SessionItem;
use rust_i18n::t;

use crate::agent_tab::settings::{AgentSettings, UI_RADIUS};
use crate::agent_tab::transcript::disclosure_row::{
    AGENT_CARD_DETAIL_SIZE, AGENT_CARD_GAP, AGENT_CARD_ICON_BLOCK, AGENT_CARD_PADDING_X,
    AgentDisclosureRow, agent_card,
};
use crate::agent_tab::transcript::format::{interrupted_status_label, worked_status_label};
#[cfg(test)]
pub(in crate::agent_tab) use crate::agent_tab::transcript::render::text_style::{
    highlight_theme_for_surface, is_dark_surface, transcript_code_block_style,
};
use crate::agent_tab::transcript::render::text_style::{markdown_view, transcript_text_style};
use crate::agent_tab::transcript::reveal::{Disclosures, RevealKey, revealed_block, revealed_part};
use crate::agent_tab::transcript::rows::{RowGap, TranscriptRow, is_run_row, row_gap};
use crate::agent_tab::transcript::{RowSpec, TranscriptView, is_work_row, working_label};

mod questions;
mod user_row;
mod work_row;

/// Edge of a transcript thumbnail, matching the composer strip so an image
/// does not change size when the message it belongs to is sent.
const TRANSCRIPT_THUMBNAIL: f32 = 56.0;

/// Share of the pane the conversation column takes, and the margin on each
/// side that leaves. A share rather than a fixed measure on a narrow pane,
/// so the column keeps a little air at its sides instead of a fixed strip
/// eating most of the width.
const TRANSCRIPT_COLUMN_FRACTION: f32 = 0.8;

fn transcript_column_margin() -> f32 {
    (1.0 - TRANSCRIPT_COLUMN_FRACTION) / 2.0
}

/// The measure the column stops growing at: 880px at the default root size,
/// which is around 90 latin characters or 45 CJK ones a line of prose. On a
/// maximised window a share of the pane would run well past that, the eye
/// loses the start of the next line on the return sweep, and every card,
/// bubble and the composer would stretch with it. Held in rems so it tracks
/// the root size the rest of the UI scales with rather than pinning a
/// physical width.
const TRANSCRIPT_COLUMN_MAX_REMS: f32 = 55.0;

/// Margin each side of the column when the reading measure is turned off and
/// the column follows the pane width instead.
const TRANSCRIPT_LOOSE_MARGIN: f32 = 40.0;

/// The column every transcript row and the composer share.
///
/// With the human-friendly layout on it is the smaller of the pane share and
/// the measure, centred in the pane. The cap sits on an inner box under the
/// percent margin because a padding cannot express "the larger of these two
/// margins". The inner box is a block centred by auto margins rather than a
/// flex item: a flex container sizes its items from their content first, and
/// a shrink-to-fit bubble measured that way wraps its CJK prose one glyph per
/// line. Block layout hands the box its definite width straight down.
///
/// With it off the column is the pane less a fixed margin each side.
pub(in crate::agent_tab) fn transcript_column(body: impl IntoElement, cx: &App) -> Div {
    if !cx.global::<AgentSettings>().human_friendly_layout {
        return div().w_full().px(px(TRANSCRIPT_LOOSE_MARGIN)).child(body);
    }

    div()
        .w_full()
        .px(relative(transcript_column_margin()))
        .child(
            div()
                .w_full()
                .max_w(rems(TRANSCRIPT_COLUMN_MAX_REMS))
                .mx_auto()
                .child(body),
        )
}

/// Three ranks of space, which is what makes a turn read as message / work /
/// message rather than as one undifferentiated stack. The widest marks where
/// one exchange ends; the middle one holds a turn's work off the prose it is
/// interleaved with, close enough that the two still read as one answer; the
/// tightest keeps the steps of a single run together.
const TRANSCRIPT_GROUP_GAP: f32 = 24.0;

const TRANSCRIPT_WORK_TEXT_GAP: f32 = 12.0;
const TRANSCRIPT_STEP_GAP: f32 = 8.0;

/// How many pixels a rank of space is worth.
fn gap_px(gap: RowGap) -> f32 {
    match gap {
        RowGap::Step => TRANSCRIPT_STEP_GAP,
        RowGap::Work => TRANSCRIPT_WORK_TEXT_GAP,
        RowGap::Group => TRANSCRIPT_GROUP_GAP,
    }
}

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
    pub(in crate::agent_tab) fn render_row(
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
        let gap = gap_px(gap);

        // The rows a run or a fold splices in open and shut as list rows of
        // their own, so each one ramps its own height and needs a height of
        // its own to ramp towards.
        let part = revealed_part(&spec);
        let now = Instant::now();

        let row = match spec {
            RowSpec::Entry { index, .. } => self.render_entry_row(index, window, cx),
            RowSpec::Work { index, .. } => self.render_work_row(index, window, cx),

            RowSpec::TurnFold {
                turn,
                row_count,
                folded,
            } => render_turn_fold(&self.disclosures, turn, row_count, folded, cx),

            RowSpec::TurnSummary {
                seconds,
                output_tokens,
            } => render_turn_summary(seconds, output_tokens, cx),

            RowSpec::Interrupted { output_tokens, .. } => render_interrupted_row(output_tokens, cx),

            RowSpec::RunToggle {
                run_start,
                tool_count,
                expanded,
            } => render_run_toggle(&self.disclosures, run_start, tool_count, expanded, cx),

            RowSpec::Working { compacting } => self.render_working_row(compacting, cx),
        };

        // Each row is laid out on its own by the virtual list, so the reading
        // column has to be re-established per row rather than once around the
        // conversation.
        let body = div()
            .w_full()
            .when(in_run, |this| {
                this.border_l(px(TRANSCRIPT_RUN_RULE))
                    .border_color(cx.theme().border)
            })
            .when(rule_carries_gap, |this| this.pb(px(gap)))
            .child(row);

        // A border is drawn at the element's own leading edge, outside any
        // padding it carries, so the inset that puts the rule on the text
        // column has to come from a level above it. Only a run needs one,
        // and only a run pays for it.
        let row = transcript_column(
            match in_run {
                true => div()
                    .w_full()
                    .pl(px(TRANSCRIPT_TEXT_INSET))
                    .child(body)
                    .into_any_element(),

                false => body.into_any_element(),
            },
            cx,
        )
        .when(!rule_carries_gap, |this| this.pb(px(gap)));

        // A row a run toggle or a turn fold spliced in grows and shrinks
        // rather than appearing and vanishing, so the conversation below it
        // travels with it the whole way instead of catching up in one jump at
        // the end. The ramp wraps the row entire — its slice of the grouping
        // rule and the space it owes the row below it — because a rule drawn
        // down to a step of no height, or a gap left where a step used to be,
        // is the part that would still jump.
        match (part, self.revealed_by(ix, now)) {
            (Some(part), Some(key)) => revealed_block(
                row,
                part,
                self.disclosures.progress(key, now),
                self.disclosures.height(part),
                self.shut_height(ix, key, now),
                cx.entity().downgrade(),
            )
            .into_any_element(),

            _ => row.into_any_element(),
        }
    }

    /// What a shutting row still occupies once it has finished shutting.
    ///
    /// When a block of rows leaves the list, the row above the block stops
    /// holding its space off the block's first row and starts holding it off
    /// whatever followed the block, and those two boundaries can rank apart:
    /// a toggle sits a step off its first step and a work rank off the reply
    /// after the run. The block's last row holds back exactly that
    /// difference, so the space the row above gains at the removal is the
    /// space the block gives up, and the removal itself moves nothing. Every
    /// row above the last owes nothing, because the boundary it leaves
    /// behind is inside the block.
    fn shut_height(&self, ix: usize, key: RevealKey, now: Instant) -> Pixels {
        if self.revealed_by(ix + 1, now) == Some(key) {
            return px(0.);
        }

        let first = (0..ix)
            .rev()
            .take_while(|cursor| self.revealed_by(*cursor, now) == Some(key))
            .last()
            .unwrap_or(ix);

        let Some(above) = first.checked_sub(1).map(|above| &self.rows[above]) else {
            return px(0.);
        };

        let below = self.rows.get(ix + 1).map(|row| &row.spec);

        let merged = row_gap(
            self.conversation.borrow().content.entries(),
            &above.spec,
            below,
        );

        px(gap_px(merged) - gap_px(above.gap))
    }

    /// The live progress line. While the backend is compacting it names that
    /// explicitly and spins: compaction produces no streamed output, so a bare
    /// seconds counter would read as a hung turn for as long as a minute.
    pub(in crate::agent_tab) fn render_working_row(
        &self,
        compacting: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(started) = self.conversation.borrow().live.started() else {
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
                                .child(t!("agent-transcript-compacting")),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(working_label(
                                    started,
                                    self.conversation.borrow().live.output_tokens(),
                                    self.conversation.borrow().live.detail(),
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
                        ShimmerText::new(working_label(
                            started,
                            self.conversation.borrow().live.output_tokens(),
                            self.conversation.borrow().live.detail(),
                        ))
                        .id("agent-working-label")
                        .highlight_color(cx.theme().foreground)
                        .peak_opacity(0.9),
                    ),
            )
            .into_any_element()
    }

    pub(in crate::agent_tab) fn render_entry_row(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let shared = self.conversation.clone();
        let conversation = shared.borrow();
        let entry = &conversation.content.entries()[index];

        match &entry.item {
            SessionItem::UserMessage { text: Some(text) } => self.render_user_row(index, text, cx),

            SessionItem::AgentMessage {
                id,
                text: Some(text),
                questions: Some(questions),
            } => {
                self.render_question_message(index, id.clone(), text.clone(), questions.clone(), cx)
            }

            SessionItem::AgentMessage {
                text: Some(text), ..
            } => self.render_agent_row(index, self.shown_reply(index, text).to_string(), cx),

            SessionItem::Error { text } => self.render_error_row(index, text.clone(), cx),

            SessionItem::Compaction { detail, .. } => {
                let detail = detail.clone();

                compaction_row::render_compaction_row(
                    self.kind,
                    self.cwd.as_deref(),
                    &self.disclosures,
                    index,
                    detail,
                    window,
                    cx,
                )
            }

            item if is_work_row(item) => self.render_work_row(index, window, cx),
            _ => div().into_any_element(),
        }
    }

    pub(in crate::agent_tab) fn render_agent_row(
        &self,
        index: usize,
        text: String,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let shared = self.conversation.clone();
        let conversation = shared.borrow();

        let attribution = conversation.content.entries()[index]
            .item
            .id()
            .and_then(|id| self.attribution.get(id));

        let cwd = attribution.map_or_else(|| self.cwd.clone(), |author| author.cwd.clone());

        h_flex()
            .id(("entry", index))
            .group("entry")
            .relative()
            .w_full()
            .items_end()
            .modern_context_menu(Self::copy_menu(cx.entity().downgrade(), index))
            .child(
                v_flex()
                    .debug_selector(move || format!("transcript-agent-{index}"))
                    .flex_1()
                    .min_w_0()
                    .px_1()
                    .when_some(attribution, |view, author| {
                        view.child(
                            div()
                                .debug_selector(move || format!("transcript-author-{index}"))
                                .mb_2()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(cx.theme().muted_foreground)
                                .child(author.name.clone()),
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

    pub(in crate::agent_tab) fn render_error_row(
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

// Rows that stand for a turn rather than for something inside one: the fold
// that hides a finished turn's work, the summary on it, an interruption, and
// the toggle for a workflow run.

/// The settled turn's work disclosure. It heads the rows it hides, so the
/// chevron keeps its usual meaning: the content it reveals is below it.
fn render_turn_fold(
    disclosures: &Disclosures,
    turn: u64,
    row_count: usize,
    folded: bool,
    cx: &mut Context<TranscriptView>,
) -> AnyElement {
    // The wording answers the click while the work under it may still be
    // leaving: a row reading "hide" through the exit it started would be
    // offering to do again what it is in the middle of doing.
    let disclosing = disclosures.is_disclosing(RevealKey::Turn(turn));

    let label = if disclosing {
        t!("agent-transcript-turn-work-hide").to_string()
    } else {
        t!("agent-transcript-turn-work", count = row_count).into_owned()
    };

    agent_card()
        .child(
            AgentDisclosureRow::new(("turn-fold", turn as usize), label.clone())
                .expanded(!folded)
                .opening(disclosures.progress(RevealKey::Turn(turn), Instant::now()))
                .type_icon(IconName::GalleryVerticalEnd)
                .accessible_label(format!(
                    "{label}. {}",
                    if disclosing {
                        t!("agent-transcript-expanded")
                    } else {
                        t!("agent-transcript-collapsed")
                    }
                ))
                .render(cx)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.toggle_disclosure(RevealKey::Turn(turn), cx)
                })),
        )
        .into_any_element()
}

/// The settled turn's closing "Worked for Ns" line, doubling as a section
/// divider (bottom hairline). Reporting only: it accounts for work the
/// disclosure above it owns, so making it clickable too would give one
/// turn two controls over the same rows.
fn render_turn_summary(
    seconds: u64,
    output_tokens: Option<u64>,
    cx: &mut Context<TranscriptView>,
) -> AnyElement {
    let label = worked_status_label(seconds, output_tokens);

    v_flex()
        .w_full()
        .gap_1()
        .child(
            div()
                .px_1()
                .text_size(px(AGENT_CARD_DETAIL_SIZE))
                .text_color(cx.theme().muted_foreground)
                .child(label),
        )
        .child(div().w_full().h(px(1.)).bg(cx.theme().border.opacity(0.6)))
        .into_any_element()
}

fn render_interrupted_row(
    output_tokens: Option<u64>,
    cx: &mut Context<TranscriptView>,
) -> AnyElement {
    v_flex()
        .w_full()
        .gap_1()
        .child(
            div()
                .px_1()
                .text_size(px(AGENT_CARD_DETAIL_SIZE))
                .text_color(cx.theme().muted_foreground)
                .child(interrupted_status_label(output_tokens)),
        )
        .child(div().w_full().h(px(1.)).bg(cx.theme().border.opacity(0.6)))
        .into_any_element()
}

/// The "+N tool calls" / "Show fewer tool calls" toggle for a work run.
fn render_run_toggle(
    disclosures: &Disclosures,
    run_start: usize,
    tool_count: usize,
    expanded: bool,
    cx: &mut Context<TranscriptView>,
) -> AnyElement {
    // The wording answers the click while the steps under it may still be
    // leaving: a toggle reading "show fewer" through the exit it started
    // would be offering to do again what it is in the middle of doing.
    let disclosing = disclosures.is_disclosing(RevealKey::Group(run_start));

    let label = if disclosing {
        t!("agent-transcript-show-fewer-tool-calls").to_string()
    } else {
        t!("agent-transcript-tool-calls", count = tool_count).into_owned()
    };

    agent_card()
        .child(
            // No type icon: the toggle names a count of steps rather than
            // being one, and its own chevron already says what it does.
            AgentDisclosureRow::new(("wl-run", run_start), label.clone())
                .expanded(expanded)
                .opening(disclosures.progress(RevealKey::Group(run_start), Instant::now()))
                .accessible_label(format!(
                    "{label}. {}",
                    if disclosing {
                        t!("agent-transcript-expanded")
                    } else {
                        t!("agent-transcript-collapsed")
                    }
                ))
                .render(cx)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.toggle_disclosure(RevealKey::Group(run_start), cx)
                })),
        )
        .into_any_element()
}

const DOT_COUNT: usize = 3;
const CYCLE_DURATION: Duration = Duration::from_millis(1_100);
const DOT_CELL_SIZE: f32 = 4.0;

/// Spacing that makes the cluster measure a card's icon block exactly. The
/// live line stands in the slot a step gives its type icon, so a cluster wider
/// than the slot would either push the label off the column a tool call's
/// title starts on or bleed into the pane's own edge inset.
const DOT_GAP: f32 =
    (AGENT_CARD_ICON_BLOCK - DOT_COUNT as f32 * DOT_CELL_SIZE) / (DOT_COUNT as f32 - 1.0);

// Dots wide enough to fill the slot on their own would run together into a
// bar, and a negative gap would overlap them; either way the indicator stops
// reading as three of anything.
const _: () = assert!(DOT_GAP > 0.0);
const DOT_MIN_SIZE: f32 = 3.2;
const DOT_MIN_OPACITY: f32 = 0.28;
const DOT_MAX_OPACITY: f32 = 0.88;

/// Three pulsing dots for an ongoing operation with no measurable completion.
#[derive(IntoElement)]
struct WorkingIndicator {
    color: Hsla,
}

impl WorkingIndicator {
    fn new(color: Hsla) -> Self {
        Self { color }
    }
}

impl RenderOnce for WorkingIndicator {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let color = self.color;

        h_flex()
            .gap(px(DOT_GAP))
            .children((0..DOT_COUNT).map(move |index| {
                // A fixed cell prevents the size pulse from shifting adjacent
                // dots.
                div()
                    .flex()
                    .size(px(DOT_CELL_SIZE))
                    .items_center()
                    .justify_center()
                    .child(div().rounded_full().bg(color).with_animation(
                        ElementId::NamedInteger("working-indicator-dot".into(), index as u64),
                        Animation::new(CYCLE_DURATION).repeat(),
                        move |dot, delta| {
                            let pulse = dot_pulse(delta, index);
                            let size = DOT_MIN_SIZE + (DOT_CELL_SIZE - DOT_MIN_SIZE) * pulse;

                            let opacity =
                                DOT_MIN_OPACITY + (DOT_MAX_OPACITY - DOT_MIN_OPACITY) * pulse;

                            dot.size(px(size)).opacity(opacity)
                        },
                    ))
            }))
    }
}

/// How far into its pulse one dot is, for a cycle position shared by all of
/// them. Each dot peaks a third of the cycle after the one before it, so the
/// swell travels along the row rather than the three breathing together.
fn dot_pulse(delta: f32, index: usize) -> f32 {
    let interval = 1.0 / DOT_COUNT as f32;
    let phase = (delta - index as f32 * interval).rem_euclid(1.0);
    let distance = phase.min(1.0 - phase);
    let pulse = (1.0 - distance / interval).clamp(0.0, 1.0);

    ease_in_out(pulse)
}

#[cfg(test)]
mod working_indicator_tests;
