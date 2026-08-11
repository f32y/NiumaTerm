//! Read-only `Background Tasks` view: the child agents a Codex or Claude Code
//! parent session spawned. Provider adapters own child lifecycle, so this
//! component only reads the latest snapshot from the active Agent pane and
//! never dispatches an operation to a child.

use std::cmp::Ordering;
use std::time::{Duration, SystemTime};

use gpui::prelude::*;
use gpui::{AnyElement, Context, ScrollHandle, SharedString, Task, WeakEntity, Window, div, px};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::tooltip::Tooltip;
use gpui_component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use nmt_agent_utils::background_task::{
    BackgroundTaskDiscoveryState, BackgroundTaskSnapshot, BackgroundTaskState,
    BackgroundTaskSummary,
};

use crate::agent_pane::AgentPane;

/// Exact label used by both the panel heading and the title-bar button.
pub(crate) const PANEL_TITLE: &str = "Background Tasks";

/// Rows shown before the section control offers the rest. Running work is the
/// part a user watches, so the finished list stays shorter per row of interest.
const COMPACT_RUNNING_ROWS: usize = 4;
const COMPACT_FINISHED_ROWS: usize = 10;

/// Elapsed labels are recomputed from stored times rather than counted, so one
/// tick per second is enough to keep a seconds display truthful.
const ELAPSED_TICK: Duration = Duration::from_secs(1);

pub(crate) struct BackgroundTasksView {
    /// The Agent pane whose children are shown. Weak because the tab can close
    /// while the panel is still mounted for its closing animation.
    target: Option<WeakEntity<AgentPane>>,
    running_expanded: bool,
    finished_expanded: bool,
    scroll: ScrollHandle,
    /// Runs only while the view is visible and at least one row has a start
    /// time to count from.
    elapsed_timer: Option<Task<()>>,
    visible: bool,
}

impl BackgroundTasksView {
    pub(crate) fn new() -> Self {
        Self {
            target: None,
            running_expanded: false,
            finished_expanded: false,
            scroll: ScrollHandle::new(),
            elapsed_timer: None,
            visible: false,
        }
    }

    /// Point the view at another parent session. Row expansion and scroll are
    /// per-conversation, so they reset rather than carrying over.
    pub(crate) fn set_target(
        &mut self,
        target: Option<WeakEntity<AgentPane>>,
        cx: &mut Context<Self>,
    ) {
        let same = match (&self.target, &target) {
            (Some(current), Some(next)) => current == next,
            (None, None) => true,
            _ => false,
        };
        if same {
            return;
        }
        self.target = target;
        self.running_expanded = false;
        self.finished_expanded = false;
        self.scroll = ScrollHandle::new();
        cx.notify();
    }

    pub(crate) fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.visible == visible {
            return;
        }
        self.visible = visible;
        if !visible {
            self.elapsed_timer = None;
        }
        cx.notify();
    }

    fn snapshot(&self, cx: &Context<Self>) -> Option<BackgroundTaskSnapshot> {
        let target = self.target.as_ref()?.upgrade()?;
        target.read(cx).background_tasks().cloned()
    }

    /// Keep one repaint task alive exactly while a visible active row has a
    /// start time; every other state leaves no timer running.
    fn sync_elapsed_timer(&mut self, needed: bool, cx: &mut Context<Self>) {
        if !needed || !self.visible {
            self.elapsed_timer = None;
            return;
        }
        if self.elapsed_timer.is_some() {
            return;
        }
        self.elapsed_timer = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(ELAPSED_TICK).await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    return;
                }
            }
        }));
    }

    fn render_section(
        &self,
        id: &'static str,
        heading: String,
        rows: &[&BackgroundTaskSummary],
        limit: usize,
        expanded: bool,
        now: SystemTime,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let shown = visible_rows(rows.len(), limit, expanded);
        let hidden = rows.len() - shown;
        let control = section_control_label(hidden, expanded).map(|label| {
            let expand = !expanded;
            let is_running = id == "background-tasks-running";
            Button::new(id)
                .ghost()
                .xsmall()
                .label(label.clone())
                .aria_label(label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    if is_running {
                        this.running_expanded = expand;
                    } else {
                        this.finished_expanded = expand;
                    }
                    cx.notify();
                }))
        });

        v_flex()
            .child(
                div()
                    .px_2()
                    .py_1()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(heading),
            )
            .children(
                rows.iter()
                    .take(shown)
                    .enumerate()
                    .map(|(index, task)| render_row(index, task, now, cx))
                    .collect::<Vec<_>>(),
            )
            .children(control.map(|control| div().px_2().pb_1().child(control)))
            .into_any_element()
    }
}

fn render_row(
    index: usize,
    task: &BackgroundTaskSummary,
    now: SystemTime,
    cx: &mut Context<BackgroundTasksView>,
) -> AnyElement {
    let theme = cx.theme();
    let color = match task.state {
        BackgroundTaskState::Failed => theme.red,
        BackgroundTaskState::NeedsInput => theme.yellow,
        BackgroundTaskState::Done => theme.green,
        BackgroundTaskState::Working | BackgroundTaskState::Starting => theme.foreground,
        BackgroundTaskState::Interrupted | BackgroundTaskState::Stopped => theme.muted_foreground,
    };
    let name = task.display_label();
    let detail = row_detail(task);
    let timing = row_timing(task, now);
    let tooltip: SharedString = format!(
        "{} · {} · {}\n{}",
        task.key.provider.label(),
        task.key.id,
        task.state.label(),
        detail
    )
    .into();

    v_flex()
        // The id exists so the row can carry a tooltip for its truncated text;
        // no click handler is attached, because the initial view reports
        // status only and dispatches no child operation.
        .id(("background-task-row", index))
        .px_2()
        .py_1()
        .gap_0p5()
        .aria_label(tooltip.clone())
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .child(div().flex_1().truncate().text_sm().child(name))
                .child(div().text_xs().text_color(color).child(task.state.label())),
        )
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(div().flex_none().child(task.key.provider.label()))
                .child(div().flex_1().truncate().child(detail))
                .children(timing.map(|timing| div().flex_none().child(timing))),
        )
        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
        .into_any_element()
}

/// Objective first, then the latest status line, then a neutral fallback so a
/// row with no optional metadata still reads as a real entry.
fn row_detail(task: &BackgroundTaskSummary) -> String {
    task.objective
        .as_deref()
        .or(task.status.as_deref())
        .or(task.last_preview.as_deref())
        .map(str::to_owned)
        .unwrap_or_else(|| "No description reported".to_string())
}

fn row_timing(task: &BackgroundTaskSummary, now: SystemTime) -> Option<String> {
    if task.state.is_terminal() {
        return task
            .completed_at
            .map(|completed| format!("{} ago", duration_label(now, completed)));
    }
    task.started_at.map(|started| duration_label(now, started))
}

/// Compact difference between two instants. Rounded down, because a label that
/// reads longer than the real elapsed time is the worse error.
fn duration_label(now: SystemTime, past: SystemTime) -> String {
    let seconds = now.duration_since(past).unwrap_or_default().as_secs();
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m", seconds / 60),
        3600..86400 => format!("{}h", seconds / 3600),
        _ => format!("{}d", seconds / 86400),
    }
}

/// Running work sorts by earliest known start so the longest-running child
/// leads; a row with no start time falls back to its update order.
fn running_rows(snapshot: &BackgroundTaskSnapshot) -> Vec<&BackgroundTaskSummary> {
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
fn finished_rows(snapshot: &BackgroundTaskSnapshot) -> Vec<&BackgroundTaskSummary> {
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

fn visible_rows(total: usize, limit: usize, expanded: bool) -> usize {
    if expanded { total } else { total.min(limit) }
}

fn section_control_label(hidden: usize, expanded: bool) -> Option<String> {
    if expanded {
        return Some("Show fewer".to_string());
    }
    (hidden > 0).then(|| format!("Show {hidden} more"))
}

fn running_heading(snapshot: &BackgroundTaskSnapshot) -> String {
    let active = snapshot.active_count();
    let needs_input = snapshot.needs_input_count();
    if needs_input > 0 {
        return format!("Running · {active} · {needs_input} need input");
    }
    format!("Running · {active}")
}

fn finished_heading(snapshot: &BackgroundTaskSnapshot) -> String {
    format!("Finished · {}", snapshot.terminal_count())
}

/// Label for the title-bar button. The count is shown only when nonzero so an
/// idle session keeps the chrome quiet.
pub(crate) fn title_bar_label(snapshot: Option<&BackgroundTaskSnapshot>) -> Option<String> {
    let active = snapshot?.active_count();
    (active > 0).then(|| active.to_string())
}

/// Whether the current session has lifecycle activity the user has not opened
/// the view for yet.
pub(crate) fn has_unseen_activity(
    snapshot: Option<&BackgroundTaskSnapshot>,
    seen_activity: Option<u64>,
) -> bool {
    snapshot.is_some_and(|snapshot| snapshot.activity > seen_activity.unwrap_or(0))
}

impl Render for BackgroundTasksView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .child(
                h_flex()
                    .px_2()
                    .py_1()
                    .items_center()
                    .border_b_1()
                    .border_color(cx.theme().sidebar_border)
                    .child(div().text_sm().child(PANEL_TITLE)),
            )
            .child(self.render_body(cx))
    }
}

impl BackgroundTasksView {
    fn render_body(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let snapshot = self.snapshot(cx);
        let now = SystemTime::now();

        let Some(snapshot) = snapshot else {
            self.sync_elapsed_timer(false, cx);
            // An Agent pane publishes its first snapshot only once the adapter
            // has something to report, so a targeted view with none yet is
            // still starting up rather than looking at the wrong kind of tab.
            return match self.target.is_some() {
                true => empty_state("Loading", "Looking for background agents…", cx),
                false => empty_state(
                    "No agent session",
                    "Open a Codex or Claude Code tab to see its background agents.",
                    cx,
                ),
            };
        };

        let running = running_rows(&snapshot);
        let finished = finished_rows(&snapshot);
        // A row without a start time has no counter to advance, so it alone
        // never justifies keeping the repaint task alive.
        let needs_timer = running.iter().any(|task| task.started_at.is_some());
        self.sync_elapsed_timer(needs_timer, cx);

        if running.is_empty() && finished.is_empty() {
            return match &snapshot.discovery {
                BackgroundTaskDiscoveryState::Unavailable { message } => empty_state(
                    "Status unavailable",
                    &format!("Background task status could not be loaded: {message}"),
                    cx,
                ),
                BackgroundTaskDiscoveryState::Loading => {
                    empty_state("Loading", "Looking for background agents…", cx)
                }
                _ => empty_state(
                    "No background agents",
                    "Child agents started by this session appear here.",
                    cx,
                ),
            };
        }

        let running_section = self.render_section(
            "background-tasks-running",
            running_heading(&snapshot),
            &running,
            COMPACT_RUNNING_ROWS,
            self.running_expanded,
            now,
            cx,
        );
        let finished_section = self.render_section(
            "background-tasks-finished",
            finished_heading(&snapshot),
            &finished,
            COMPACT_FINISHED_ROWS,
            self.finished_expanded,
            now,
            cx,
        );

        v_flex()
            .id("background-tasks-body")
            .flex_1()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .child(running_section)
            .child(div().border_t_1().border_color(cx.theme().sidebar_border))
            .child(finished_section)
            .into_any_element()
    }
}

fn empty_state(title: &str, detail: &str, cx: &Context<BackgroundTasksView>) -> AnyElement {
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

#[cfg(test)]
mod tests;
