use nmt_i18n::i18n;

use crate::agent::*;

impl AgentPane {
    /// State that outlives the running turn, in one strip above the composer:
    /// whether the backend is planning rather than working, and the objective
    /// it keeps returning to.
    ///
    /// Both belong here rather than in the transcript because neither is
    /// something the conversation said — they are what the next turn will be
    /// governed by, which is the question the composer is asking.
    pub(in crate::agent::view) fn render_session_state(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        if !self.plan_mode && self.goal.is_none() {
            return None;
        }

        let plan = self.plan_mode.then(|| {
            h_flex()
                .flex_none()
                .gap_1()
                .items_center()
                .text_color(cx.theme().warning)
                .child(Icon::new(IconName::Map).size_3())
                .child(i18n("agent-session-plan-mode"))
        });

        // The round counter is what says how much of the goal's own budget is
        // left, so it travels with the objective rather than waiting for the
        // goal to run out on its own.
        let goal = self.goal.as_ref().map(|goal| {
            let rounds = (goal.max_rounds > 0)
                .then(|| {
                    i18n("agent-session-goal-rounds")
                        .replace("{used}", &goal.rounds_started.to_string())
                        .replace("{total}", &goal.max_rounds.to_string())
                })
                .unwrap_or_default();

            h_flex()
                .flex_1()
                .min_w_0()
                .gap_1()
                .items_center()
                .child(
                    Icon::new(IconName::CircleCheck)
                        .size_3()
                        .flex_none()
                        .text_color(cx.theme().muted_foreground.opacity(0.7)),
                )
                .child(div().min_w_0().truncate().child(goal.objective.clone()))
                .when(!rounds.is_empty(), |this| {
                    this.child(
                        div()
                            .flex_none()
                            .text_color(cx.theme().muted_foreground.opacity(0.6))
                            .child(rounds),
                    )
                })
        });

        Some(
            h_flex()
                .w_full()
                .px_3()
                .py_1p5()
                .gap_2()
                .items_center()
                .border_b_1()
                .border_color(cx.theme().border.opacity(0.6))
                .bg(cx.theme().muted.opacity(0.3))
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .children(plan)
                .children(goal),
        )
    }
}
