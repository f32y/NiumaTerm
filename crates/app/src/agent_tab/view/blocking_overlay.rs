use std::borrow::Cow;
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{AnyElement, Context, FontWeight, Window, div, px};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::progress::ProgressCircle;
use gpui_component::spinner::Spinner;
use gpui_component::{ActiveTheme as _, IconName, Sizable as _, h_flex, v_flex};
use rust_i18n::t;

use crate::agent_tab::fade::{Fade, FrostedLayer};
use crate::agent_tab::session::UpdateSuspension;
use crate::agent_tab::{AgentPane, AgentPaneEvent};

/// The frosted layer that holds the whole pane while the harness starts or
/// while an update owns the backend. It owns the fade that carries the layer
/// in and out.
#[derive(Default)]
pub(crate) struct BlockingOverlay {
    fade: Fade,
}

impl BlockingOverlay {
    /// The layer over the pane, while it is showing `body` or still fading out
    /// after the state it showed has ended. The body belongs to that state, so
    /// a fading layer carries nothing.
    pub(crate) fn render(
        &mut self,
        body: Option<AnyElement>,
        now: Instant,
        window: &mut Window,
        cx: &mut Context<AgentPane>,
    ) -> Option<AnyElement> {
        let frost = self.fade.drive(body.is_some(), now, window, cx);

        (!frost.gone()).then(|| {
            FrostedLayer::new(frost)
                .padded()
                .children(body)
                .into_any_element()
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum UpdateOverlayPhase {
    Stopping,
    Updating,
    Reconnecting,
}

impl UpdateOverlayPhase {
    fn label(self) -> Cow<'static, str> {
        match self {
            Self::Stopping => t!("agent-update-stopping-label"),
            Self::Updating => t!("agent-update-updating-label"),
            Self::Reconnecting => t!("agent-update-reconnecting-label"),
        }
    }
}

pub(super) fn update_overlay_phase(state: &UpdateSuspension) -> Option<UpdateOverlayPhase> {
    match state {
        UpdateSuspension::Stopping => Some(UpdateOverlayPhase::Stopping),
        UpdateSuspension::Updating => Some(UpdateOverlayPhase::Updating),
        UpdateSuspension::Reconnecting => Some(UpdateOverlayPhase::Reconnecting),
        UpdateSuspension::Waiting | UpdateSuspension::Failed(_) => None,
    }
}

/// The strip over the transcript for the update states the tab stays
/// usable in.
pub(crate) fn update_banner(
    suspension: Option<&UpdateSuspension>,
    cx: &mut Context<AgentPane>,
) -> Option<AnyElement> {
    suspension.and_then(|state| {
        // The phases that tear the backend down and bring it back own the
        // whole surface through `update_overlay`, so the strip only
        // covers the two states the tab stays usable in.
        let (label, detail, failed) = match state {
            UpdateSuspension::Waiting => (
                t!("agent-update-waiting-label"),
                t!("agent-update-waiting-detail"),
                false,
            ),
            UpdateSuspension::Failed(message) => (
                t!("agent-update-reconnect-failed-label"),
                message.as_str().into(),
                true,
            ),
            UpdateSuspension::Stopping
            | UpdateSuspension::Updating
            | UpdateSuspension::Reconnecting => return None,
        };

        let banner = h_flex()
            .w_full()
            .px_4()
            .py_2()
            .gap_3()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(if failed {
                cx.theme().danger.opacity(0.12)
            } else {
                cx.theme().primary.opacity(0.10)
            })
            .child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(if failed {
                        cx.theme().danger
                    } else {
                        cx.theme().primary
                    })
                    .child(label),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(detail.to_string()),
            )
            .when(failed, |row| {
                row.child(
                    Button::new("agent-update-retry")
                        .outline()
                        .small()
                        .label(t!("agent-update-retry"))
                        .on_click(cx.listener(|this, _, _, cx| this.retry_update_recovery(cx))),
                )
                .child(
                    Button::new("agent-update-new-session")
                        .danger()
                        .small()
                        .label(t!("agent-update-start-new-session"))
                        .on_click(
                            cx.listener(|this, _, _, cx| this.start_new_after_update_failure(cx)),
                        ),
                )
            })
            .into_any_element();

        Some(banner)
    })
}

/// What the blocking layer shows while the update transaction owns the
/// backend: input would go nowhere, and the transcript underneath is a
/// stale snapshot of a conversation that is about to be replayed.
pub(crate) fn update_overlay(
    suspension: Option<&UpdateSuspension>,
    cx: &mut Context<AgentPane>,
) -> Option<AnyElement> {
    let label = update_overlay_phase(suspension?)?.label();

    let body = v_flex()
        .items_center()
        .gap_3()
        .child(
            Spinner::new()
                .icon(IconName::LoaderCircle)
                .with_size(px(22.))
                .color(cx.theme().primary),
        )
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(cx.theme().foreground)
                .child(label),
        );

    Some(body.into_any_element())
}

/// What the blocking layer shows during the harness's start. When a start
/// counts as still running is the session's own call.
///
/// A start that failed keeps the layer and answers with the two things
/// left to do, because the pane behind it has no conversation to return
/// to: the transcript holds one error row and nothing else.
/// `failure` is the start's error, if it failed, and `starting` whether the
/// session still counts the start as running.
pub(crate) fn start_overlay(
    failure: Option<String>,
    starting: bool,
    cx: &mut Context<AgentPane>,
) -> Option<AnyElement> {
    if failure.is_none() && !starting {
        return None;
    }

    let body = match &failure {
        Some(message) => v_flex()
            .max_w(px(420.))
            .items_center()
            .gap_4()
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().danger)
                    .child(message.clone()),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("agent-start-retry")
                            .primary()
                            .small()
                            .label(t!("agent-start-retry"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.start_session(None, cx);
                            })),
                    )
                    .child(
                        Button::new("agent-start-close-tab")
                            .outline()
                            .small()
                            .label(t!("agent-start-close-tab"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.emit_event(AgentPaneEvent::CloseRequested, cx);
                            })),
                    ),
            ),
        None => v_flex()
            .items_center()
            .gap_3()
            .child(
                ProgressCircle::new("agent-start-progress")
                    .loading(true)
                    .loading_duration(Duration::from_millis(1_200))
                    .size(px(22.))
                    .color(cx.theme().primary),
            )
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(cx.theme().foreground)
                    .child(t!("agent-start-starting")),
            ),
    };

    Some(body.into_any_element())
}
