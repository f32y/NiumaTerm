use crate::agent_pane::context_usage::ContextUsageIndicator;
use crate::agent_pane::*;

impl AgentPane {
    pub(in crate::agent_pane::view) fn render_approval_panel(
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
                        .child("PENDING APPROVAL"),
                )
                .child(
                    div()
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
                                .label("Cancel turn")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.respond_approval("cancel", cx)
                                })),
                        )
                        .child(
                            Button::new("approval-decline")
                                .outline()
                                .label("Decline")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.respond_approval("decline", cx)
                                })),
                        )
                        .child(
                            Button::new("approval-session")
                                .outline()
                                .label("Always allow this session")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.respond_approval("acceptForSession", cx)
                                })),
                        )
                        .child(
                            Button::new("approval-accept")
                                .primary()
                                .label("Approve once")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.respond_approval("accept", cx)
                                })),
                        ),
                )
                .into_any_element()
        })
    }

    pub(in crate::agent_pane::view) fn render_update_banner(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        self.update_suspension.as_ref().map(|state| {
            let (label, detail, failed) = match state {
                UpdateSuspension::Waiting => (
                    "WAITING FOR UPDATE",
                    "New provider input is paused until this tab becomes recoverably idle.",
                    false,
                ),
                UpdateSuspension::Stopping => (
                    "STOPPING AGENT",
                    "The provider process is closing while this tab stays open.",
                    false,
                ),
                UpdateSuspension::Updating => (
                    "UPDATING PROVIDER",
                    "The provider binary is being updated; transcript and draft are retained.",
                    false,
                ),
                UpdateSuspension::Reconnecting => (
                    "RECONNECTING",
                    "Restoring this tab to its provider conversation.",
                    false,
                ),
                UpdateSuspension::Failed(message) => ("RECONNECT FAILED", message.as_str(), true),
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
                            .label("Retry")
                            .on_click(cx.listener(|this, _, _, cx| this.retry_update_recovery(cx))),
                    )
                    .child(
                        Button::new("agent-update-new-session")
                            .danger()
                            .small()
                            .label("Start new session")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.start_new_after_update_failure(cx)
                            })),
                    )
                })
                .into_any_element()
        })
    }

    pub(in crate::agent_pane::view) fn render_composer_status(
        &self,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (branch, branch_opacity) = self.git_branch_poll.presentation();

        let usage = self
            .context_window_usage
            .map(|usage| ContextUsageIndicator::new(usage, self.context_composition.clone()));

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
                    .children(usage),
            )
            .into_any_element()
    }
}
