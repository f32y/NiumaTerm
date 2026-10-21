//! The quiet readouts a composer card shows around its input: the harness's
//! directory limits, the prompts queued behind a running turn, and how long
//! the conversation has been sitting.

use std::collections::VecDeque;

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, div, px};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::tooltip::Tooltip;
use gpui_component::{ActiveTheme as _, Icon, IconName, Sizable as _, h_flex, v_flex};
use nmt_agent::chat::{PromptPlacement, QueuedPrompt};
use nmt_agent::session::capabilities::AgentCapabilities as _;
use nmt_agent::{AgentWorkspace, MultiRootAccess};
use rust_i18n::t;

use crate::agent_tab::AgentPane;
use crate::agent_tab::composer::visible_prompt;
use crate::agent_tab::profile::AgentKind;
use crate::agent_tab::transcript::{LAST_RESPONSE_LIMIT, last_response_label};

/// What an Agent Tab has to disclose about the directories its harness cannot
/// reach, or `None` when there is nothing to disclose. Derived from the
/// harness's declared access and the workspace alone, so a permission-preset
/// change can neither raise nor clear it: choosing a broader preset widens what
/// the harness may do inside the one root it has, and does not give it
/// selected-root isolation across the others.
pub(crate) fn multi_root_notice(kind: AgentKind, workspace: &AgentWorkspace) -> Option<String> {
    if kind.caps().multi_root_access == MultiRootAccess::Full || !workspace.is_multi_root() {
        return None;
    }

    Some(
        t!(
            "agent-multi-root-primary-only",
            agent = kind.display(),
            path = workspace.primary().unwrap_or_default(),
            count = workspace.additional().len()
        )
        .into_owned(),
    )
}

/// A strip naming the workspace directories the installed harness cannot
/// use. It is not dismissible and appears before the first prompt, because
/// a user who attached three directories would otherwise only discover the
/// reduction from the agent failing to find a file.
pub(crate) fn multi_root_strip(notice: String, cx: &App) -> AnyElement {
    h_flex()
        .w_full()
        .px_4()
        .py_2()
        .gap_3()
        .border_b_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().warning.opacity(0.10))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(notice),
        )
        .into_any_element()
}

/// One waiting prompt on one line. A prompt spanning several lines is folded
/// into one so every waiting row costs the composer the same height.
///
/// The wording names which turn reads the message: a correction the running
/// turn takes at its next step is not the same promise as one that waits for
/// the turn to end, and only the label tells the user which they got.
pub(super) fn queued_message_label(prompt: &QueuedPrompt) -> String {
    let text = visible_prompt(&prompt.text)
        .lines()
        .collect::<Vec<_>>()
        .join(" ");

    match prompt.placement {
        PromptPlacement::Steering => {
            t!("agent-history-steering-message", text = &text).into_owned()
        }
        PromptPlacement::Queued => t!("agent-history-queued-message", text = &text).into_owned(),
    }
}

/// The prompts waiting behind the running turn, one row each, above the
/// composer. A row whose backend named it has controls that drop it again or
/// move it into the running turn; one this side is only remembering has none,
/// because there is nothing on the backend such a control could address.
pub(crate) fn queued_prompts(
    pending: &VecDeque<QueuedPrompt>,
    cx: &mut Context<AgentPane>,
) -> Option<impl IntoElement + use<>> {
    if pending.is_empty() {
        return None;
    }

    Some(
        v_flex()
            .w_full()
            .px_3()
            .py_1p5()
            .gap_0p5()
            .children(pending.iter().enumerate().map(|(index, prompt)| {
                let mut row = h_flex().w_full().gap_1().items_center().child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(queued_message_label(prompt)),
                );

                if let Some(id) = prompt.id.clone() {
                    // Moving a prompt into the running turn is offered only for
                    // one the harness is holding for the next turn: a prompt
                    // already read by the running turn has nothing to move.
                    if prompt.placement == PromptPlacement::Queued {
                        let moved = id.clone();

                        row = row.child(
                            Button::new(("queued-prompt-steer", index))
                                .ghost()
                                .xsmall()
                                .icon(IconName::Zap)
                                .tooltip(t!("agent-history-queued-steer"))
                                .accessibility_label(t!("agent-history-queued-steer"))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.promote_queued_prompt(&moved, cx)
                                })),
                        );
                    }

                    row = row.child(
                        Button::new(("queued-prompt-remove", index))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Close)
                            .tooltip(t!("agent-history-queued-remove"))
                            .accessibility_label(t!("agent-history-queued-remove"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.remove_queued_prompt(&id, cx)
                            })),
                    );
                }

                row
            })),
    )
}

/// Edge of the mark. Set to the size of a settings pill's own glyph, so the
/// row it stands in keeps one glyph size across its whole width.
const LAST_RESPONSE_MARK: f32 = 12.0;

/// How far into the window a conversation has to have drifted before the
/// composer says so, and before it says so in the danger colour. The window is
/// the one a provider's prompt cache is expected to hold, so the first mark
/// says the next message is going to start costing more than the last one did,
/// and the second says it is about to cost a full re-read of the context.
const LAST_RESPONSE_WARNING: f32 = 0.5;

const LAST_RESPONSE_DANGER: f32 = 0.9;

/// How loudly the composer marks a conversation that has been sitting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LastResponseTone {
    Warning,
    Danger,
}

/// The mark a settled conversation shows, from how long it has been sitting.
///
/// Under half the window there is nothing to say: a conversation picked
/// up that soon costs what it would have cost immediately, and a reading that
/// is always on screen is one the eye stops seeing. Past the window the answer
/// stops changing, which is the same answer as the last reading inside it.
pub(super) fn last_response_tone(seconds: u64) -> Option<LastResponseTone> {
    let drift = seconds as f32 / LAST_RESPONSE_LIMIT.as_secs() as f32;

    if drift >= LAST_RESPONSE_DANGER {
        Some(LastResponseTone::Danger)
    } else if drift >= LAST_RESPONSE_WARNING {
        Some(LastResponseTone::Warning)
    } else {
        None
    }
}

/// How long ago the agent last answered, beside the composer's controls.
///
/// Drawn as a mark, not as a reading: the number itself only
/// matters once it is large enough to change what the next message costs,
/// and until then a line of text beside the settings is one more thing to
/// read past on the way to sending. The wording it used to carry is on the
/// mark's tooltip, and in its accessible label.
///
/// `seconds` is how long ago the answer settled.
pub(crate) fn last_response_mark(seconds: u64, cx: &App) -> Option<AnyElement> {
    let color = match last_response_tone(seconds)? {
        LastResponseTone::Warning => cx.theme().warning,
        LastResponseTone::Danger => cx.theme().danger,
    };

    let label = last_response_label(seconds);
    let tooltip = label.clone();

    Some(
        div()
            .id("agent-last-response")
            .flex_none()
            .flex()
            .items_center()
            .aria_label(label)
            .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
            .child(
                Icon::new(IconName::TriangleAlert)
                    .size(px(LAST_RESPONSE_MARK))
                    .text_color(color),
            )
            .into_any_element(),
    )
}
