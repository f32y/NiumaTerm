#[cfg(test)]
mod tests;

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{AnyElement, App, Bounds, Context, FontWeight, Pixels, ScrollHandle, Window, div, px};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::spinner::Spinner;
use gpui_component::{ActiveTheme as _, Icon, IconName, Sizable as _, h_flex, v_flex};
use nmt_agent::progress::{GoalStatus, Task, TaskList, TaskStatus};
use rust_i18n::t;

use crate::agent_tab::AgentPane;
use crate::agent_tab::fade::Fade;
use crate::agent_tab::view::composer_layout::composer_panel;

/// Opening the details pushes the transcript up, and a moving edge needs longer
/// than an opacity change before the eye reads it as travel instead of a jump.
const DETAILS_DURATION: Duration = Duration::from_millis(160);

pub(crate) struct ProgressPanel {
    expanded: bool,
    scroll: ScrollHandle,
    details_fade: Fade,

    /// The details' laid-out height, recorded during prepaint. The height ramp
    /// runs towards it, and the details always lay out at full size so the
    /// ramp never measures its own animated box.
    details_height: Rc<Cell<Option<Pixels>>>,
}

impl Default for ProgressPanel {
    fn default() -> Self {
        Self {
            expanded: false,
            scroll: ScrollHandle::default(),
            details_fade: Fade::lasting(DETAILS_DURATION),
            details_height: Rc::default(),
        }
    }
}

impl ProgressPanel {
    pub(crate) fn render(
        &mut self,
        goal: Option<&GoalStatus>,
        tasks: Option<&TaskList>,
        plan_mode: bool,
        window: &mut Window,
        cx: &mut Context<AgentPane>,
    ) -> Option<AnyElement> {
        let empty = TaskList::default();

        // A finished list has nothing left to steer by, and the transcript
        // already shows it under the reply that finished it.
        let tasks = tasks
            .filter(|tasks| !tasks.all_completed())
            .unwrap_or(&empty);

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
            .debug_selector(|| "agent-progress-header".into())
            .w_full()
            .px_3()
            .py_1()
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

        let frame = self
            .details_fade
            .drive(expanded, Instant::now(), window, cx);

        let details = (!frame.gone()).then(|| {
            let body = v_flex()
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
                .children(tasks.items.iter().map(|task| task_row(task, true, cx)));

            let recorded = Rc::clone(&self.details_height);

            // Recorded without notifying: only a running ramp reads the value,
            // and it is already asking for frames.
            let measure = move |bounds: Vec<Bounds<Pixels>>, _: &mut Window, _: &mut App| {
                if let Some(bounds) = bounds.first() {
                    recorded.set(Some(bounds.size.height));
                }
            };

            let progress = frame.progress();

            if progress >= 1.0 {
                return div()
                    .w_full()
                    .on_children_prepainted(measure)
                    .child(body)
                    .into_any_element();
            }

            // While the ramp runs, the box is what grows and the details sit
            // out of flow inside it at their full height. A first opening has
            // nothing measured yet and starts from zero for one frame.
            div()
                .w_full()
                .relative()
                .h(self.details_height.get().unwrap_or_default() * progress)
                .overflow_hidden()
                .opacity(progress)
                .on_children_prepainted(measure)
                .child(div().absolute().top_0().w_full().child(body))
                .into_any_element()
        });

        Some(
            div()
                .w_full()
                .flex()
                .justify_center()
                .child(
                    composer_panel(cx)
                        .debug_selector(|| "agent-progress-panel".into())
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

/// Edge of a task's state mark, matching the small icon size.
const TASK_MARK: f32 = 12.0;

/// One task as a checklist line: its state as a mark, then the title and
/// whatever else the provider said about it. A `live` list is the one the
/// agent is working through, so its running task turns; a list recorded in
/// the transcript shows the state it captured and stays still.
pub(crate) fn task_row(task: &Task, live: bool, cx: &App) -> AnyElement {
    let mark = match task.status {
        TaskStatus::InProgress if live => Spinner::new()
            .icon(IconName::LoaderCircle)
            .with_size(px(TASK_MARK))
            .color(cx.theme().primary)
            .into_any_element(),
        TaskStatus::InProgress => Icon::new(IconName::LoaderCircle)
            .size(px(TASK_MARK))
            .text_color(cx.theme().primary)
            .into_any_element(),
        TaskStatus::Pending => Icon::new(IconName::Minus)
            .size(px(TASK_MARK))
            .text_color(cx.theme().muted_foreground)
            .into_any_element(),
        TaskStatus::Completed => Icon::new(IconName::Check)
            .size(px(TASK_MARK))
            .text_color(cx.theme().success)
            .into_any_element(),
    };

    h_flex()
        .w_full()
        .items_start()
        .gap_2()
        .child(div().flex_none().mt_0p5().child(mark))
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
