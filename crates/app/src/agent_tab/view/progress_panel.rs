#[cfg(test)]
mod tests;

use gpui::prelude::*;
use gpui::{AnyElement, Context, FontWeight, Hsla, ScrollHandle, div, px, relative};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{ActiveTheme as _, Icon, IconName, Sizable as _, h_flex, v_flex};
use nmt_agent::progress::{GoalStatus, Task, TaskList, TaskStatus};
use rust_i18n::t;

use crate::agent_tab::AgentPane;
use crate::agent_tab::settings::UI_RADIUS;

#[derive(Default)]
pub(crate) struct ProgressPanel {
    expanded: bool,
    scroll: ScrollHandle,
}

impl ProgressPanel {
    pub(crate) fn render(
        &self,
        goal: Option<&GoalStatus>,
        tasks: Option<&TaskList>,
        plan_mode: bool,
        background: Hsla,
        cx: &mut Context<AgentPane>,
    ) -> Option<AnyElement> {
        let empty = TaskList::default();
        let tasks = tasks.unwrap_or(&empty);

        if goal.is_none() && tasks.items.is_empty() && !plan_mode {
            return None;
        }

        let summary = goal.map(|goal| goal.objective.clone()).unwrap_or_else(|| {
            tasks
                .items
                .iter()
                .find(|task| task.status == TaskStatus::InProgress)
                .or_else(|| {
                    tasks
                        .items
                        .iter()
                        .find(|task| task.status == TaskStatus::Pending)
                })
                .map(|task| task.title.clone())
                .unwrap_or_else(|| {
                    t!(if plan_mode {
                        "agent-session-plan-mode"
                    } else {
                        "agent-progress-tasks"
                    })
                    .into_owned()
                })
        });

        let tally = tasks
            .tally()
            .map(|(done, total)| format!("{done}/{total}"))
            .or_else(|| {
                let goal = goal.filter(|goal| goal.max_rounds > 0)?;

                Some(
                    t!(
                        "agent-session-goal-rounds",
                        used = goal.rounds_started,
                        total = goal.max_rounds
                    )
                    .into_owned(),
                )
            });

        let expanded = self.expanded;

        let header = h_flex()
            .w_full()
            .px_3()
            .pt_1()
            .pb_1()
            .gap_2()
            .items_center()
            .child(Icon::new(IconName::Map).size_3().flex_none())
            .child(div().flex_1().min_w_0().truncate().child(summary))
            .children(tally.map(|tally| div().flex_none().child(tally)))
            .child(
                div()
                    .debug_selector(|| "agent-progress-toggle".into())
                    .flex_none()
                    .child(
                        Button::new("agent-progress-toggle")
                            .ghost()
                            .xsmall()
                            .icon(if expanded {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronUp
                            })
                            .tooltip(t!(if expanded {
                                "agent-progress-collapse"
                            } else {
                                "agent-progress-expand"
                            }))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.progress_panel.expanded = !this.progress_panel.expanded;

                                cx.notify();
                            })),
                    ),
            );

        let details = expanded.then(|| {
            v_flex()
                .id("agent-progress-details")
                .debug_selector(|| "agent-progress-details".into())
                .w_full()
                .max_h(px(260.))
                .overflow_y_scroll()
                .track_scroll(&self.scroll)
                .px_3()
                .pb_2()
                .gap_2()
                .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                .when(plan_mode, |this| {
                    this.child(
                        div()
                            .text_color(cx.theme().warning)
                            .child(t!("agent-session-plan-mode")),
                    )
                })
                .children(goal.map(|goal| goal_details(goal, cx)))
                .children(
                    tasks
                        .explanation
                        .as_ref()
                        .map(|text| div().child(text.clone())),
                )
                .children(tasks.items.iter().map(|task| task_row(task, cx)))
        });

        Some(
            div()
                .w_full()
                .flex()
                .justify_center()
                .child(
                    v_flex()
                        .debug_selector(|| "agent-progress-panel".into())
                        .w(relative(0.95))
                        .rounded_t(UI_RADIUS)
                        .border_1()
                        .border_b_0()
                        .border_color(cx.theme().border.opacity(0.6))
                        .bg(background.blend(cx.theme().muted))
                        .pb(px(20.))
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(header)
                        .children(details),
                )
                .into_any_element(),
        )
    }
}

fn goal_details(goal: &GoalStatus, cx: &Context<AgentPane>) -> AnyElement {
    let (label, color) = match goal.phase.as_str() {
        "active" => (t!("agent-progress-active"), cx.theme().primary),
        "complete" => (t!("agent-progress-complete"), cx.theme().success),
        "paused" => (t!("agent-progress-paused"), cx.theme().warning),
        "blocked" => (t!("agent-progress-blocked"), cx.theme().danger),
        "failed" => (t!("agent-progress-failed"), cx.theme().danger),
        "usageLimited" | "usage_limited" | "budgetLimited" | "budget_limited" => {
            (t!("agent-progress-limited"), cx.theme().warning)
        }
        _ => (goal.phase.clone().into(), cx.theme().muted_foreground),
    };

    let mut counters = Vec::new();

    if goal.max_rounds > 0 {
        counters.push(
            t!(
                "agent-session-goal-rounds",
                used = goal.rounds_started,
                total = goal.max_rounds
            )
            .into_owned(),
        );
    } else if goal.rounds_started > 0 {
        counters.push(t!("agent-progress-rounds", count = goal.rounds_started).into_owned());
    }

    if let Some(used) = goal.tokens_used {
        counters.push(match goal.token_budget {
            Some(total) => {
                t!("agent-progress-token-budget", used = used, total = total).into_owned()
            }
            None => t!("agent-progress-tokens", count = used).into_owned(),
        });
    }

    if let Some(seconds) = goal.elapsed_seconds {
        counters.push(t!("agent-progress-seconds", count = seconds).into_owned());
    }

    v_flex()
        .w_full()
        .gap_1()
        .child(
            h_flex()
                .gap_2()
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(t!("agent-progress-goal")),
                )
                .child(div().text_color(color).child(label)),
        )
        .child(
            div()
                .debug_selector(|| "agent-progress-objective".into())
                .w_full()
                .whitespace_normal()
                .child(goal.objective.clone()),
        )
        .when(!counters.is_empty(), |this| {
            this.child(counters.join(" · "))
        })
        .children(
            goal.reason
                .as_ref()
                .map(|reason| div().child(reason.clone())),
        )
        .into_any_element()
}

fn task_row(task: &Task, cx: &Context<AgentPane>) -> AnyElement {
    let (icon, color) = match task.status {
        TaskStatus::Pending => (IconName::Minus, cx.theme().muted_foreground),
        TaskStatus::InProgress => (IconName::LoaderCircle, cx.theme().primary),
        TaskStatus::Completed => (IconName::CircleCheck, cx.theme().success),
    };

    h_flex()
        .w_full()
        .items_start()
        .gap_2()
        .child(
            Icon::new(icon)
                .size_3()
                .flex_none()
                .text_color(color)
                .mt_0p5(),
        )
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_1()
                .child(div().whitespace_normal().child(task.title.clone()))
                .children(
                    task.description
                        .as_ref()
                        .filter(|description| {
                            !description.is_empty() && **description != task.title
                        })
                        .map(|description| div().whitespace_normal().child(description.clone())),
                )
                .children(
                    task.owner
                        .as_ref()
                        .map(|owner| div().child(t!("agent-progress-owner", owner = owner))),
                )
                .when(!task.blocked_by.is_empty(), |this| {
                    this.child(t!(
                        "agent-progress-dependencies",
                        tasks = task.blocked_by.join(", ")
                    ))
                }),
        )
        .into_any_element()
}
