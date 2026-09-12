use gpui::prelude::*;
use gpui::{Context, IntoElement, div};
use gpui_component::{ActiveTheme as _, Icon, IconName, h_flex};
use nmt_agent::chat::GoalStatus;
use rust_i18n::t;

use crate::agent_tab::AgentPane;

/// State that outlives the running turn, drawn as one strip above the
/// composer: whether the backend is planning rather than working, and the
/// objective it keeps returning to.
///
/// Both belong here rather than in the transcript because neither is
/// something the conversation said — they are what the next turn will be
/// governed by, which is the question the composer is asking. They are set and
/// cleared together, and drawn nowhere else.
#[derive(Default)]
pub(in crate::agent_tab) struct SessionStateBadge;

impl SessionStateBadge {
    pub(in crate::agent_tab) fn render(
        &self,
        goal: &Option<GoalStatus>,
        plan_mode: bool,
        cx: &mut Context<AgentPane>,
    ) -> Option<impl IntoElement + use<>> {
        if !plan_mode && goal.is_none() {
            return None;
        }

        let plan = plan_mode.then(|| {
            h_flex()
                .flex_none()
                .gap_1()
                .items_center()
                .text_color(cx.theme().warning)
                .child(Icon::new(IconName::Map).size_3())
                .child(t!("agent-session-plan-mode"))
        });

        // The round counter is what says how much of the goal's own budget is
        // left, so it travels with the objective rather than waiting for the
        // goal to run out on its own.
        let goal = goal.as_ref().map(|goal| {
            let rounds = if goal.max_rounds > 0 {
                {
                    t!(
                        "agent-session-goal-rounds",
                        used = goal.rounds_started,
                        total = goal.max_rounds
                    )
                    .into_owned()
                }
            } else {
                Default::default()
            };

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
