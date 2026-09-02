//! One task as a row, and the two sections rows are grouped into.
//!
//! Running tasks are what the panel is for, so they are listed first and in
//! full; finished ones are kept because a result is still worth reading, and
//! collapse behind a count once there are more than the panel has room for.

use std::cmp::Ordering;
use std::time::SystemTime;

use gpui::prelude::*;
use gpui::{AnyElement, Context, Hsla, SharedString, div, px};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use nmt_agent_utils::background_task::{
    BackgroundTaskKind, BackgroundTaskSnapshot, BackgroundTaskState, BackgroundTaskSummary,
};
use nmt_i18n::i18n;

use crate::ui::background_tasks::{BackgroundTasksView, StopTaskIcon};

pub(super) fn render_row(
    index: usize,
    task: &BackgroundTaskSummary,
    now: SystemTime,
    cx: &mut Context<BackgroundTasksView>,
) -> AnyElement {
    let theme = cx.theme();
    let color = state_color(task.state, cx);
    let name = task.display_label();
    let detail = row_detail(task);
    let timing = row_timing(task, now);
    let state_label = background_task_state_label(task.state);
    // Everything the row shows visually, in one string. A screen reader
    // announces the row as a whole, so it needs the parts the layout separates
    // into two lines plus the child id, which is not rendered anywhere.
    let description: SharedString = format!(
        "{} · {} · {} · {}\n{}",
        task.key.provider.label(),
        background_task_kind_label(task.kind),
        task.key.id,
        state_label,
        detail
    )
    .into();

    let key = task.key.clone();
    let stop_key = task.key.clone();

    // Whether a child can be stopped depends on identifiers the provider may
    // not have published yet, not on the lifecycle state alone, so the control
    // follows what the snapshot reports rather than being inferred here.
    let stop = task.can_stop.then(|| {
        // Keyed by the child rather than by row position: the two sections
        // enumerate independently, so a positional id is not unique across them.
        Button::new(SharedString::from(format!(
            "background-task-stop-{}",
            task.key.id
        )))
        .ghost()
        .xsmall()
        .icon(StopTaskIcon)
        .tooltip(i18n("tasks-background-stop-tooltip"))
        .aria_label(i18n("tasks-background-stop-tooltip"))
        .on_click(cx.listener(move |this, _, _, cx| {
            // The row opens the child's conversation, so a click that was
            // meant for Stop must not also navigate.
            cx.stop_propagation();
            this.stop_task(&stop_key, cx);
        }))
    });

    h_flex()
        // Activating a row opens that child's own conversation.
        .id(("background-task-row", index))
        .px_2()
        .py_1()
        .gap_2()
        // The control is centred against the row as a whole rather than aligned
        // to either text line, which keeps it steady whether the row has a
        // timing label or not.
        .items_center()
        .cursor_pointer()
        .hover(|this| this.bg(theme.list_hover))
        .aria_label(description)
        .on_click(cx.listener(move |this, _, _, cx| this.open_detail(key.clone(), cx)))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(div().flex_1().truncate().text_sm().child(name))
                        .child(div().text_xs().text_color(color).child(state_label)),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(div().flex_none().child(task.key.provider.label()))
                        .child(
                            div()
                                .flex_none()
                                .child(background_task_kind_label(task.kind)),
                        )
                        .child(div().flex_1().truncate().child(detail))
                        .children(timing.map(|timing| div().flex_none().child(timing))),
                ),
        )
        .children(stop.map(|stop| div().flex_none().child(stop)))
        .into_any_element()
}

/// Objective first, then the latest status line, then a neutral fallback so a
/// row with no optional metadata still reads as a real entry.
pub(super) fn state_color(state: BackgroundTaskState, cx: &Context<BackgroundTasksView>) -> Hsla {
    let theme = cx.theme();
    match state {
        BackgroundTaskState::Failed => theme.red,
        BackgroundTaskState::NeedsInput => theme.yellow,
        BackgroundTaskState::Done => theme.green,
        BackgroundTaskState::Working | BackgroundTaskState::Starting => theme.foreground,
        BackgroundTaskState::Interrupted | BackgroundTaskState::Stopped => theme.muted_foreground,
    }
}

pub(super) fn row_detail(task: &BackgroundTaskSummary) -> String {
    task.objective
        .as_deref()
        .or(task.status.as_deref())
        .or(task.last_preview.as_deref())
        .map(str::to_owned)
        .unwrap_or_else(|| i18n("tasks-background-no-description").to_string())
}

pub(super) fn row_timing(task: &BackgroundTaskSummary, now: SystemTime) -> Option<String> {
    if task.state.is_terminal() {
        return task.completed_at.map(|completed| {
            i18n("tasks-background-finished-ago")
                .replace("{duration}", &duration_label(now, completed))
        });
    }
    task.started_at.map(|started| duration_label(now, started))
}

/// Compact difference between two instants. Rounded down, because a label that
/// reads longer than the real elapsed time is the worse error.
pub(super) fn duration_label(now: SystemTime, past: SystemTime) -> String {
    let seconds = now.duration_since(past).unwrap_or_default().as_secs();
    match seconds {
        0..60 => i18n("tasks-background-duration-seconds").replace("{count}", &seconds.to_string()),
        60..3600 => i18n("tasks-background-duration-minutes")
            .replace("{count}", &(seconds / 60).to_string()),
        3600..86400 => i18n("tasks-background-duration-hours")
            .replace("{count}", &(seconds / 3600).to_string()),
        _ => i18n("tasks-background-duration-days")
            .replace("{count}", &(seconds / 86400).to_string()),
    }
}

/// What kind of work a row is. Shown beside the provider because a child agent
/// and a background command differ in what the row's detail and its output
/// mean, which the description text alone does not say.
pub(super) fn background_task_kind_label(kind: BackgroundTaskKind) -> &'static str {
    match kind {
        BackgroundTaskKind::Agent => i18n("tasks-background-kind-agent"),
        BackgroundTaskKind::Shell => i18n("tasks-background-kind-shell"),
    }
}

pub(super) fn background_task_state_label(state: BackgroundTaskState) -> &'static str {
    match state {
        BackgroundTaskState::Starting => i18n("tasks-background-state-starting"),
        BackgroundTaskState::Working => i18n("tasks-background-state-working"),
        BackgroundTaskState::NeedsInput => i18n("tasks-background-state-needs-input"),
        BackgroundTaskState::Done => i18n("tasks-background-state-done"),
        BackgroundTaskState::Interrupted => i18n("tasks-background-state-interrupted"),
        BackgroundTaskState::Stopped => i18n("tasks-background-state-stopped"),
        BackgroundTaskState::Failed => i18n("tasks-background-state-failed"),
    }
}

/// Running work sorts by earliest known start so the longest-running child
/// leads; a row with no start time falls back to its update order.
pub(super) fn running_rows(snapshot: &BackgroundTaskSnapshot) -> Vec<&BackgroundTaskSummary> {
    let mut rows: Vec<_> = snapshot
        .tasks
        .iter()
        .filter(|task| task.state.is_active())
        .collect();
    rows.sort_by(|left, right| match (left.started_at, right.started_at) {
        (Some(left_start), Some(right_start)) => left_start.cmp(&right_start),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => left.sequence.cmp(&right.sequence),
    });
    rows
}

/// Finished work sorts by most recent completion, which is the order a user
/// scans for "what just ended".
pub(super) fn finished_rows(snapshot: &BackgroundTaskSnapshot) -> Vec<&BackgroundTaskSummary> {
    let mut rows: Vec<_> = snapshot
        .tasks
        .iter()
        .filter(|task| task.state.is_terminal())
        .collect();
    rows.sort_by(
        |left, right| match (left.completed_at, right.completed_at) {
            (Some(left_end), Some(right_end)) => right_end.cmp(&left_end),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => right.sequence.cmp(&left.sequence),
        },
    );
    rows
}

pub(super) fn visible_rows(total: usize, limit: usize, expanded: bool) -> usize {
    if expanded { total } else { total.min(limit) }
}

pub(super) fn section_control_label(hidden: usize, expanded: bool) -> Option<String> {
    if expanded {
        return Some(i18n("tasks-background-show-fewer").to_string());
    }
    (hidden > 0).then(|| i18n("tasks-background-show-more").replace("{count}", &hidden.to_string()))
}

pub(super) fn running_heading(snapshot: &BackgroundTaskSnapshot) -> String {
    let active = snapshot.active_count();
    let needs_input = snapshot.needs_input_count();
    if needs_input > 0 {
        return i18n("tasks-background-heading-running-needs-input")
            .replace("{count}", &active.to_string())
            .replace("{needs}", &needs_input.to_string());
    }
    i18n("tasks-background-heading-running").replace("{count}", &active.to_string())
}

pub(super) fn finished_heading(snapshot: &BackgroundTaskSnapshot) -> String {
    i18n("tasks-background-heading-finished")
        .replace("{count}", &snapshot.terminal_count().to_string())
}

pub(super) fn empty_state(
    title: &str,
    detail: &str,
    cx: &Context<BackgroundTasksView>,
) -> AnyElement {
    v_flex()
        .flex_1()
        .items_center()
        .justify_center()
        .gap_1()
        .px(px(16.0))
        .child(div().text_sm().child(title.to_string()))
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(detail.to_string()),
        )
        .into_any_element()
}
