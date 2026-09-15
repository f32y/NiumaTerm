//! A step of the agent's work as a card in the transcript.

use std::time::Instant;

use gpui::prelude::*;
use gpui::{AnyElement, Context, div, px};
use gpui_component::modern_menu::ModernMenuExt as _;
use gpui_component::{ActiveTheme as _, IconName};
use nmt_agent::chat::Item as SessionItem;
use rust_i18n::t;

use crate::agent_tab::transcript::disclosure_row::{
    AGENT_CARD_BODY_PADDING_Y, AGENT_CARD_DETAIL_SIZE, AGENT_CARD_PADDING_X, AGENT_CARD_RADIUS,
    AGENT_DISCLOSURE_DETAIL_INSET, AgentCardTone, AgentDisclosureRow, agent_card,
};
use crate::agent_tab::transcript::render::menus::copy_entry_menu;
use crate::agent_tab::transcript::reveal::{Disclosures, RevealKey, RevealedPart, revealed_block};
use crate::agent_tab::transcript::{
    TranscriptView, command_execution_heading, command_failure_reason,
};

/// What a work card says about one step.
pub(crate) struct WorkStep<'a> {
    pub(crate) icon: IconName,
    pub(crate) heading: String,
    pub(crate) reason: Option<String>,
    pub(crate) status: Option<String>,
    pub(crate) detail: Option<&'a str>,
}

/// One step of the work log as its card states it: the icon for its kind, a
/// heading, why it failed when it did, its outcome, and the detail the card
/// discloses. `None` for an entry that is not a step.
pub(crate) fn work_step(item: &SessionItem) -> Option<WorkStep<'_>> {
    let (icon, heading, reason, status, detail) = match item {
        SessionItem::CommandExecution {
            purpose,
            aggregated_output,
            status,
            exit_code,
            ..
        } => {
            // Belt and braces: a non-zero exit code is a failure even if
            // the provider reported the execution as completed.
            let state = status.as_deref().unwrap_or("inProgress");

            let failed =
                matches!(state, "failed" | "declined") || exit_code.is_some_and(|code| code != 0);

            let state = if failed { "failed" } else { state };

            let detail = aggregated_output.as_deref().unwrap_or("");

            (
                IconName::SquareTerminal,
                command_execution_heading(purpose.as_deref()).to_string(),
                failed
                    .then(|| command_failure_reason(aggregated_output.as_deref()))
                    .flatten(),
                Some(state.to_string()),
                Some(detail),
            )
        }
        SessionItem::FileChange {
            paths,
            diff,
            status,
            ..
        } => (
            IconName::File,
            t!("agent-transcript-edit-paths", paths = paths).into_owned(),
            None,
            Some(status.as_deref().unwrap_or("inProgress").to_string()),
            diff.as_deref().filter(|diff| !diff.trim().is_empty()),
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
            output.as_deref().filter(|output| !output.trim().is_empty()),
        ),
        SessionItem::Reasoning { summary, .. } => (
            IconName::Bot,
            t!("agent-transcript-thinking").to_string(),
            None,
            None,
            summary.as_deref().filter(|text| !text.trim().is_empty()),
        ),
        _ => return None,
    };

    Some(WorkStep {
        icon,
        heading,
        reason,
        status,
        detail,
    })
}

/// The card for step `index`: icon block, heading and outcome mark, with the
/// failure reason and the disclosed `body` under them. `body` is present only
/// while the step is expanded.
pub(crate) fn work_card(
    index: usize,
    step: WorkStep<'_>,
    disclosures: &Disclosures,
    body: Option<AnyElement>,
    cx: &mut Context<TranscriptView>,
) -> AnyElement {
    let WorkStep {
        icon,
        heading,
        reason,
        status,
        detail,
    } = step;

    let expandable = detail.is_some();
    let expanded = expandable && disclosures.row_expanded(index);

    let detail_reveal = disclosures.progress(RevealKey::Row(index), Instant::now());

    let detail_part = RevealedPart::Block(RevealKey::Row(index));
    let detail_height = disclosures.height(detail_part);
    let detail_view = cx.entity().downgrade();

    let status_label = match status.as_deref() {
        Some("failed") => t!("agent-transcript-status-failed"),
        Some("declined") => t!("agent-transcript-status-declined"),
        Some("completed") => t!("agent-transcript-status-completed"),
        Some("inProgress") => t!("agent-transcript-status-in-progress"),
        Some(status) => status.into(),
        None => t!("agent-transcript-no-status"),
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
            if disclosures.is_disclosing(RevealKey::Row(index)) {
                t!("agent-transcript-accessibility-expanded")
            } else {
                t!("agent-transcript-accessibility-collapsed")
            }
        } else {
            "".into()
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

    let header = header.render(cx).when(expandable, |this| {
        this.on_click(
            cx.listener(move |this, _, _, cx| this.toggle_disclosure(RevealKey::Row(index), cx)),
        )
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
            .children(body.map(|body| {
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
        .modern_context_menu(copy_entry_menu(cx.entity().downgrade(), index))
        .child(header)
        .children(card_body)
        .into_any_element()
}
