#[cfg(test)]
pub(super) use crate::agent_tab::transcript::render::text_style::highlight_theme_for_surface;
#[cfg(test)]
pub(super) use crate::agent_tab::transcript::render::text_style::is_dark_surface;
#[cfg(test)]
pub(super) use crate::agent_tab::transcript::render::text_style::transcript_code_block_style;

pub(super) mod image_preview;
pub(super) mod text_style;

pub(super) mod compaction_row;

#[cfg(test)]
mod working_indicator_tests;

use crate::agent_tab::settings::AgentSettings;
use crate::agent_tab::transcript::TranscriptView;
use crate::agent_tab::transcript::disclosure_row::{
    AGENT_CARD_DETAIL_SIZE, AGENT_CARD_ICON_BLOCK, AgentDisclosureRow, agent_card,
};
use crate::agent_tab::transcript::format::{interrupted_status_label, worked_status_label};
use crate::agent_tab::transcript::reveal::{Disclosures, RevealKey};
use crate::agent_tab::transcript::rows::RowGap;
use gpui::prelude::*;
use gpui::{
    Animation, AnimationExt as _, AnyElement, App, Context, Div, ElementId, Hsla, RenderOnce,
    Window, div, ease_in_out, px, relative, rems,
};
use gpui_component::{ActiveTheme as _, IconName, h_flex, v_flex};
use rust_i18n::t;
use std::time::{Duration, Instant};

/// Edge of a transcript thumbnail, matching the composer strip so an image
/// does not change size when the message it belongs to is sent.
pub(super) const TRANSCRIPT_THUMBNAIL: f32 = 56.0;

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
pub(crate) fn transcript_column(body: impl IntoElement, cx: &App) -> Div {
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
pub(super) fn gap_px(gap: RowGap) -> f32 {
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
pub(super) const TRANSCRIPT_RUN_RULE: f32 = 2.0;

/// Where the conversation's own text starts inside the reading column, which
/// every prose row and status line sets on itself. The run rule stands on
/// that edge rather than left of it, so a run reads as part of the column
/// instead of hanging off it.
pub(super) const TRANSCRIPT_TEXT_INSET: f32 = 4.0;

/// Leading for transcript text, as a multiple of the font size. Conversation
/// prose is read in paragraphs rather than scanned line by line the way
/// terminal output is, so it is set looser than the chrome around it.
pub(super) const TRANSCRIPT_LINE_HEIGHT: f32 = 1.6;

// Rows that stand for a turn rather than for something inside one: the fold
// that hides a finished turn's work, the summary on it, an interruption, and
// the toggle for a workflow run.

/// The settled turn's work disclosure. It heads the rows it hides, so the
/// chevron keeps its usual meaning: the content it reveals is below it.
pub(super) fn render_turn_fold(
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
pub(super) fn render_turn_summary(
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

pub(super) fn render_interrupted_row(
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
pub(super) fn render_run_toggle(
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
pub(super) struct WorkingIndicator {
    color: Hsla,
}

impl WorkingIndicator {
    pub(super) fn new(color: Hsla) -> Self {
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
