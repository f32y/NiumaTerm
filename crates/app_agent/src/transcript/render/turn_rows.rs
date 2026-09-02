//! Rows that stand for a turn rather than for something inside one: the fold
//! that hides a finished turn's work, the summary on it, an interruption, and
//! the toggle for a workflow run.

use std::time::Instant;

use gpui::prelude::*;
use gpui::{AnyElement, Context, div, px};
use gpui_component::{ActiveTheme as _, IconName, v_flex};
use nmt_i18n::i18n;

use crate::transcript::TranscriptView;
use crate::transcript::disclosure_row::{AGENT_CARD_DETAIL_SIZE, AgentDisclosureRow, agent_card};
use crate::transcript::format::{interrupted_status_label, worked_status_label};
use crate::transcript::reveal::RevealKey;

impl TranscriptView {
    /// The settled turn's work disclosure. It heads the rows it hides, so the
    /// chevron keeps its usual meaning: the content it reveals is below it.
    pub(crate) fn render_turn_fold(
        &self,
        turn: u64,
        row_count: usize,
        folded: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let label = if folded {
            i18n("agent-transcript-turn-work").replace("{count}", &row_count.to_string())
        } else {
            i18n("agent-transcript-turn-work-hide").to_string()
        };

        agent_card()
            .child(
                AgentDisclosureRow::new(("turn-fold", turn as usize), label.clone())
                    .expanded(!folded)
                    .type_icon(IconName::GalleryVerticalEnd)
                    .accessible_label(format!(
                        "{label}. {}",
                        if folded {
                            i18n("agent-transcript-collapsed")
                        } else {
                            i18n("agent-transcript-expanded")
                        }
                    ))
                    .render(cx)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.toggled_turns.insert(turn) {
                            this.toggled_turns.remove(&turn);
                        }
                        cx.notify();
                    })),
            )
            .into_any_element()
    }

    /// The settled turn's closing "Worked for Ns" line, doubling as a section
    /// divider (bottom hairline). Reporting only: it accounts for work the
    /// disclosure above it owns, so making it clickable too would give one
    /// turn two controls over the same rows.
    pub(crate) fn render_turn_summary(
        &self,
        seconds: u64,
        output_tokens: Option<u64>,
        cx: &mut Context<Self>,
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

    pub(crate) fn render_interrupted_row(
        &self,
        output_tokens: Option<u64>,
        cx: &mut Context<Self>,
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
    pub(crate) fn render_run_toggle(
        &self,
        run_start: usize,
        tool_count: usize,
        expanded: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // The wording answers the click while the steps under it may still be
        // leaving: a toggle reading "show fewer" through the exit it started
        // would be offering to do again what it is in the middle of doing.
        let disclosing = self.is_disclosing(RevealKey::Group(run_start));
        let label = if disclosing {
            i18n("agent-transcript-show-fewer-tool-calls").to_string()
        } else {
            i18n("agent-transcript-tool-calls").replace("{count}", &tool_count.to_string())
        };

        agent_card()
            .child(
                // No type icon: the toggle names a count of steps rather than
                // being one, and its own chevron already says what it does.
                AgentDisclosureRow::new(("wl-run", run_start), label.clone())
                    .expanded(expanded)
                    .opening(
                        self.reveals
                            .progress(RevealKey::Group(run_start), 0, Instant::now()),
                    )
                    .accessible_label(format!(
                        "{label}. {}",
                        if disclosing {
                            i18n("agent-transcript-expanded")
                        } else {
                            i18n("agent-transcript-collapsed")
                        }
                    ))
                    .render(cx)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.toggle_disclosure(RevealKey::Group(run_start), cx)
                    })),
            )
            .into_any_element()
    }
}
