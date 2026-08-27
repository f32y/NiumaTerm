use nmt_i18n::i18n;

use crate::agent::context_usage::{ContextUsageIndicator, cache_hit_percent};
use crate::agent::view::blocking_overlay::BlockingOverlay;
use crate::agent::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::agent::view) enum UpdateOverlayPhase {
    Stopping,
    Updating,
    Reconnecting,
}

impl UpdateOverlayPhase {
    fn label(self) -> &'static str {
        match self {
            Self::Stopping => i18n("agent-update-stopping-label"),
            Self::Updating => i18n("agent-update-updating-label"),
            Self::Reconnecting => i18n("agent-update-reconnecting-label"),
        }
    }
}

pub(in crate::agent::view) fn update_overlay_phase(
    state: &UpdateSuspension,
) -> Option<UpdateOverlayPhase> {
    match state {
        UpdateSuspension::Stopping => Some(UpdateOverlayPhase::Stopping),
        UpdateSuspension::Updating => Some(UpdateOverlayPhase::Updating),
        UpdateSuspension::Reconnecting => Some(UpdateOverlayPhase::Reconnecting),
        UpdateSuspension::Waiting | UpdateSuspension::Failed(_) => None,
    }
}

/// Sub-second latencies are the interesting ones, and a reading like `0.8s`
/// hides how much of a second it was; past a second the tenth is enough.
fn latency_readout(latency: Duration) -> String {
    if latency < Duration::from_secs(1) {
        format!("{}ms", latency.as_millis())
    } else {
        format!("{:.1}s", latency.as_secs_f64())
    }
}

/// The composer's one-line account of the conversation: how many turns it has
/// run, how many actions the newest turn took, how long that turn waited for
/// its first output, and how much of the input the provider had cached. Each
/// part is dropped rather than shown as a zero when nothing reports it, and a
/// conversation that has not run a turn yet reports nothing at all.
pub(in crate::agent::view) fn composer_stats_label(
    turns: u64,
    steps: usize,
    first_output: Option<Duration>,
    cache_hit: Option<u64>,
) -> Option<String> {
    if turns == 0 {
        return None;
    }

    let mut parts = vec![i18n("agent-status-turns").replace("{count}", &turns.to_string())];

    if steps > 0 {
        parts.push(i18n("agent-status-steps").replace("{count}", &steps.to_string()));
    }
    if let Some(first_output) = first_output {
        parts.push(
            i18n("agent-status-first-output").replace("{value}", &latency_readout(first_output)),
        );
    }
    if let Some(percent) = cache_hit {
        parts.push(i18n("agent-status-cache-hit").replace("{percent}", &percent.to_string()));
    }

    Some(parts.join(" · "))
}

impl AgentPane {
    pub(in crate::agent::view) fn render_approval_panel(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        self.pending_approval.as_ref().map(|approval| {
            v_flex()
                .w_full()
                .px_4()
                .py_3()
                .gap_2()
                .border_b_1()
                .border_color(cx.theme().border.opacity(0.65))
                .bg(cx.theme().muted.opacity(0.2))
                .child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(cx.theme().muted_foreground)
                        .child(i18n("agent-approval-pending")),
                )
                .child(
                    div()
                        // The description carries whatever the request holds:
                        // a whole plan for ExitPlanMode, a full command line
                        // for Bash. Without a ceiling the card grows past the
                        // pane and the decision buttons below it are clipped
                        // away, leaving the turn unanswerable.
                        .id("approval-description")
                        .max_h(px(256.))
                        .overflow_y_scroll()
                        .px_3()
                        .py_2()
                        .rounded(UI_RADIUS)
                        .border_1()
                        .border_color(cx.theme().border)
                        .bg(cx.theme().background.opacity(0.7))
                        .text_sm()
                        .child(approval.clone()),
                )
                .child(
                    h_flex()
                        .justify_end()
                        .gap_2()
                        .child(
                            Button::new("approval-cancel")
                                .ghost()
                                .label(i18n("agent-approval-cancel-turn"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.respond_approval("cancel", cx)
                                })),
                        )
                        .child(
                            Button::new("approval-decline")
                                .outline()
                                .label(i18n("agent-approval-decline"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.respond_approval("decline", cx)
                                })),
                        )
                        // Offered only where it means something. A harness that
                        // can answer just this one call would quietly turn a
                        // session-wide grant into a single-use one.
                        .when(self.kind.caps().session_scoped_approval, |this| {
                            this.child(
                                Button::new("approval-session")
                                    .outline()
                                    .label(i18n("agent-approval-allow-session"))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.respond_approval("acceptForSession", cx)
                                    })),
                            )
                        })
                        .child(
                            Button::new("approval-accept")
                                .primary()
                                .label(i18n("agent-approval-approve-once"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.respond_approval("accept", cx)
                                })),
                        ),
                )
                .into_any_element()
        })
    }

    /// The `AskUserQuestion` card. The provider caps a batch at four questions
    /// of two to four options, so every question renders expanded rather than
    /// paged: the user sees the whole ask before answering any of it.
    pub(in crate::agent::view) fn render_question_panel(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let prompt = self.pending_questions.as_ref()?;
        let complete = prompt.is_complete();

        // Collected eagerly: each row needs `cx` mutably to build its listeners,
        // which a lazy iterator would still be holding while the surrounding
        // card reads the theme.
        let questions: Vec<AnyElement> = prompt
            .questions
            .iter()
            .enumerate()
            .map(|(index, question)| {
                let header = question.header.as_ref().map(|header| {
                    div()
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(cx.theme().muted_foreground)
                        .child(header.clone())
                });

                v_flex()
                    .w_full()
                    .gap_1p5()
                    .children(header)
                    .child(div().text_sm().child(question.question.clone()))
                    .child(self.render_question_options(index, question, prompt, cx))
                    .into_any_element()
            })
            .collect();

        Some(
            v_flex()
                .w_full()
                .px_4()
                .py_3()
                .gap_3()
                .border_b_1()
                .border_color(cx.theme().border.opacity(0.65))
                .bg(cx.theme().muted.opacity(0.2))
                .child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(cx.theme().muted_foreground)
                        .child(i18n("agent-question-pending")),
                )
                .children(questions)
                .child(
                    h_flex()
                        .justify_end()
                        .gap_2()
                        .child(
                            Button::new("question-skip")
                                .ghost()
                                .label(i18n("agent-question-skip"))
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.respond_questions(false, cx)),
                                ),
                        )
                        .child(
                            Button::new("question-submit")
                                .primary()
                                // Submitting a partial set would report the
                                // unanswered questions as refusals, so the
                                // button waits for every question.
                                .disabled(!complete)
                                .label(i18n("agent-question-submit"))
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.respond_questions(true, cx)),
                                ),
                        ),
                )
                .into_any_element(),
        )
    }

    fn render_question_options(
        &self,
        question_index: usize,
        question: &Question,
        prompt: &QuestionPrompt,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let describe = |option: &QuestionOption| {
            option
                .description
                .as_ref()
                .filter(|description| !description.is_empty())
                .map_or_else(
                    || option.label.clone(),
                    |description| format!("{} — {description}", option.label),
                )
        };

        // One id namespace per question, so option ids stay unique across the
        // card without assuming how many options a question carries.
        let group: SharedString = format!("agent-question-{question_index}").into();
        let multi_select = question.multi_select;

        // Each option is drawn on its own so the row the arrow keys are on can
        // carry the highlight. A radio group would render its own children and
        // leave no way to mark one of them.
        v_flex()
            .gap_1()
            .children(question.options.iter().enumerate().map(|(index, option)| {
                let control: AnyElement = if multi_select {
                    Checkbox::new((group.clone(), index))
                        .label(describe(option))
                        .checked(prompt.is_selected(question_index, index))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.toggle_question_option(question_index, index, cx)
                        }))
                        .into_any_element()
                } else {
                    // One answer only, which the pick itself enforces by
                    // replacing rather than adding.
                    Radio::new((group.clone(), index))
                        .label(describe(option))
                        .checked(prompt.is_selected(question_index, index))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.toggle_question_option(question_index, index, cx)
                        }))
                        .into_any_element()
                };

                div()
                    .w_full()
                    .px_1p5()
                    .py_0p5()
                    .rounded(UI_RADIUS)
                    .when(prompt.is_focused(question_index, index), |this| {
                        this.bg(cx.theme().list_active)
                    })
                    .child(control)
            }))
            .into_any_element()
    }

    /// A strip naming the workspace directories the installed harness cannot
    /// use. It is not dismissible and appears before the first prompt, because
    /// a user who attached three directories would otherwise only discover the
    /// reduction from the agent failing to find a file.
    pub(in crate::agent::view) fn render_multi_root_notice(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let notice = multi_root_notice(self.kind, self.configured_workspace())?;

        Some(
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
                .into_any_element(),
        )
    }

    pub(in crate::agent::view) fn render_update_banner(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        self.runtime.update_suspension.as_ref().and_then(|state| {
            // The phases that tear the backend down and bring it back own the
            // whole surface through `render_update_overlay`, so the strip only
            // covers the two states the tab stays usable in.
            let (label, detail, failed) = match state {
                UpdateSuspension::Waiting => (
                    i18n("agent-update-waiting-label"),
                    i18n("agent-update-waiting-detail"),
                    false,
                ),
                UpdateSuspension::Failed(message) => (
                    i18n("agent-update-reconnect-failed-label"),
                    message.as_str(),
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
                            .label(i18n("agent-update-retry"))
                            .on_click(cx.listener(|this, _, _, cx| this.retry_update_recovery(cx))),
                    )
                    .child(
                        Button::new("agent-update-new-session")
                            .danger()
                            .small()
                            .label(i18n("agent-update-start-new-session"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.start_new_after_update_failure(cx)
                            })),
                    )
                })
                .into_any_element();
            Some(banner)
        })
    }

    /// Covers the surface while the update transaction owns the backend: input
    /// would go nowhere, and the transcript underneath is a stale snapshot of a
    /// conversation that is about to be replayed.
    pub(in crate::agent::view) fn render_update_overlay(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let label = update_overlay_phase(self.runtime.update_suspension.as_ref()?)?.label();

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

        Some(BlockingOverlay::new(body).into_any_element())
    }

    /// The harness's start, over the tab it is starting in.
    ///
    /// Only the DeepSeek pane wears one: its host is a Node process that may
    /// still be fetching its package, so the gap is long enough that an
    /// unexplained dead tab is the alternative. The CLI backends are running
    /// within a frame or two, where an overlay would only flash.
    ///
    /// A start that failed keeps the overlay and answers with the two things
    /// left to do, because the pane behind it has no conversation to return
    /// to: the transcript holds one error row and nothing else.
    pub(in crate::agent::view) fn render_start_overlay(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.wears_start_overlay() {
            return None;
        }

        let failure = self.runtime.start_failure.clone();
        if failure.is_none() && !self.runtime.start_overlay_visible {
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
                                .label(i18n("agent-start-retry"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.start_session(None, cx);
                                })),
                        )
                        .child(
                            Button::new("agent-start-close-tab")
                                .outline()
                                .small()
                                .label(i18n("agent-start-close-tab"))
                                .on_click(cx.listener(|_, _, _, cx| {
                                    cx.emit(AgentPaneEvent::CloseRequested);
                                })),
                        ),
                ),
            None => v_flex()
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
                        .child(i18n("agent-start-starting")),
                ),
        };

        Some(BlockingOverlay::new(body).padded().into_any_element())
    }

    pub(in crate::agent::view) fn render_composer_status(
        &self,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (branch, branch_opacity) = self.git_branch_poll.presentation();

        let usage = self.context_window_usage.map(|usage| {
            ContextUsageIndicator::new(usage, self.context_composition.clone(), self.session_stats)
        });

        // A backend that folds the count from its whole log is authoritative:
        // this side's counter sees only the turns it replayed, and a replay is
        // one page rather than the conversation.
        let turns = self
            .session_stats
            .map(|stats| stats.turns)
            .unwrap_or(self.turn.seq);

        let stats = composer_stats_label(
            turns,
            self.transcript.read(cx).turn_steps(self.turn.seq),
            self.turn.first_output_latency,
            self.context_window_usage.and_then(cache_hit_percent),
        );

        v_flex()
            .w(relative(0.95))
            .rounded_b(UI_RADIUS)
            .border_1()
            .border_t_0()
            .border_color(cx.theme().border.opacity(0.6))
            .bg(cx.theme().popover)
            .pt(px(18.))
            .child(
                h_flex()
                    .w_full()
                    .min_h(px(26.))
                    .px_4()
                    .pt(px(6.))
                    .pb(px(6.))
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .text_xs()
                    .child(
                        h_flex()
                            .min_w_0()
                            .gap_1p5()
                            .items_center()
                            .text_color(cx.theme().muted_foreground.opacity(branch_opacity))
                            .child(Icon::new(IconName::GitBranch).size_3())
                            .child(div().min_w_0().truncate().child(branch)),
                    )
                    .child(
                        // The readouts belong with the context indicator rather
                        // than centered between it and the branch: both report
                        // what the conversation has spent, and a variable-width
                        // group in the middle would drift as its parts appear.
                        h_flex()
                            .flex_none()
                            .gap_3()
                            .items_center()
                            .children(stats.map(|stats| {
                                div()
                                    .id("agent-composer-stats")
                                    .aria_label(
                                        i18n("agent-status-accessibility")
                                            .replace("{stats}", &stats),
                                    )
                                    .text_color(cx.theme().muted_foreground.opacity(0.72))
                                    .child(stats)
                            }))
                            .children(usage),
                    ),
            )
            .into_any_element()
    }
}

/// What an Agent Tab has to disclose about the directories its harness cannot
/// reach, or `None` when there is nothing to disclose. Derived from the
/// harness's declared access and the workspace alone, so a permission-preset
/// change can neither raise nor clear it: choosing a broader preset widens what
/// the harness may do inside the one root it has, and does not give it
/// selected-root isolation across the others.
pub(in crate::agent::view) fn multi_root_notice(
    kind: AgentKind,
    workspace: &AgentWorkspace,
) -> Option<String> {
    if kind.caps().multi_root_access == MultiRootAccess::Full || !workspace.is_multi_root() {
        return None;
    }

    Some(
        i18n("agent-multi-root-primary-only")
            .replace("{agent}", kind.display())
            .replace("{path}", workspace.primary().unwrap_or_default())
            .replace("{count}", &workspace.additional().len().to_string()),
    )
}
