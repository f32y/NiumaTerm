use nmt_i18n::i18n;

use crate::agent::context_usage::{ContextUsageIndicator, cache_hit_percent};
use crate::agent::*;

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

    pub(in crate::agent::view) fn render_update_banner(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        self.update_suspension.as_ref().map(|state| {
            let (label, detail, failed) = match state {
                UpdateSuspension::Waiting => (
                    i18n("agent-update-waiting-label"),
                    i18n("agent-update-waiting-detail"),
                    false,
                ),
                UpdateSuspension::Stopping => (
                    i18n("agent-update-stopping-label"),
                    i18n("agent-update-stopping-detail"),
                    false,
                ),
                UpdateSuspension::Updating => (
                    i18n("agent-update-updating-label"),
                    i18n("agent-update-updating-detail"),
                    false,
                ),
                UpdateSuspension::Reconnecting => (
                    i18n("agent-update-reconnecting-label"),
                    i18n("agent-update-reconnecting-detail"),
                    false,
                ),
                UpdateSuspension::Failed(message) => (
                    i18n("agent-update-reconnect-failed-label"),
                    message.as_str(),
                    true,
                ),
            };

            h_flex()
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
                .into_any_element()
        })
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
            .unwrap_or(self.turn_seq);

        let stats = composer_stats_label(
            turns,
            self.transcript.read(cx).turn_steps(self.turn_seq),
            self.first_output_latency,
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
