//! `Background Tasks` view: the child agents a Codex or Claude Code parent
//! session spawned. Provider adapters own child lifecycle, so this component
//! reads the latest snapshot from the active Agent pane and dispatches nothing
//! to a child beyond the one operation a snapshot reports as available — a row
//! offers Stop only while its adapter says that child can be stopped.

use std::cmp::Ordering;
use std::time::{Duration, SystemTime};

use gpui::prelude::*;
use gpui::{
    AnyElement, Context, Entity, Hsla, ScrollHandle, SharedString, Task, WeakEntity, Window, div,
    px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{
    ActiveTheme as _, IconName, IconNamed, Sizable as _, StyledExt as _, h_flex, v_flex,
};
use nmt_agent_utils::background_task::{
    BackgroundTaskDiscoveryState, BackgroundTaskKey, BackgroundTaskSnapshot, BackgroundTaskState,
    BackgroundTaskSummary, BackgroundTaskTranscriptState,
};
use nmt_app_agent::AgentPane;
use nmt_app_agent::transcript::TranscriptView;
use nmt_i18n::i18n;

use crate::ui::AppSettings;
use crate::ui::composition::panel_header;

/// Rows shown before the section control offers the rest. Running work is the
/// part a user watches, so the finished list stays shorter per row of interest.
const COMPACT_RUNNING_ROWS: usize = 4;
const COMPACT_FINISHED_ROWS: usize = 10;

/// Elapsed labels are recomputed from stored times rather than counted, so one
/// tick per second is enough to keep a seconds display truthful.
const ELAPSED_TICK: Duration = Duration::from_secs(1);

/// The square the composer's own Stop control uses, so stopping one child reads
/// as the same action as stopping a turn.
#[derive(Clone, Copy)]
struct StopTaskIcon;

impl IconNamed for StopTaskIcon {
    fn path(self) -> SharedString {
        "icons/stop.svg".into()
    }
}

/// What the panel is showing. One child at a time replaces the list rather
/// than opening beside it, so the shared right-side area still holds one view.
#[derive(Clone, Debug, PartialEq, Eq)]
enum PanelMode {
    List,
    Detail {
        key: BackgroundTaskKey,
        /// Section expansion restored when the user goes back, so returning
        /// lands on what they were reading.
        running_expanded: bool,
        finished_expanded: bool,
    },
}

impl PanelMode {
    fn detail_key(&self) -> Option<&BackgroundTaskKey> {
        match self {
            Self::Detail { key, .. } => Some(key),
            Self::List => None,
        }
    }

    /// Open one child, remembering the list state behind it.
    fn open(&mut self, key: BackgroundTaskKey, running_expanded: bool, finished_expanded: bool) {
        *self = Self::Detail {
            key,
            running_expanded,
            finished_expanded,
        };
    }

    /// Return to the list. Reports the section expansion to restore, or `None`
    /// when the list was already showing.
    fn close(&mut self) -> Option<(bool, bool)> {
        let Self::Detail {
            running_expanded,
            finished_expanded,
            ..
        } = self
        else {
            return None;
        };
        let restored = (*running_expanded, *finished_expanded);
        *self = Self::List;
        Some(restored)
    }
}

pub(crate) struct BackgroundTasksView {
    mode: PanelMode,
    /// The open child's conversation, rendered by the same component the Agent
    /// pane uses. Its own instance, so expansion and scroll belong to this
    /// child rather than to the parent conversation.
    detail_transcript: Option<Entity<TranscriptView>>,
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
            mode: PanelMode::List,
            detail_transcript: None,
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
        // A child belongs to one parent session, so pointing at another one
        // returns to the list rather than keeping that child on screen.
        self.mode = PanelMode::List;
        self.detail_transcript = None;
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
                let alive = this.update(cx, |this, cx| {
                    this.refresh_open_child(cx);
                    cx.notify();
                });
                if alive.is_err() {
                    return;
                }
            }
        }));
    }

    /// Re-read the open child while it is still working. Claude Code writes a
    /// child's own turns to that child's file instead of publishing them on
    /// the parent stream, so no event arrives to repaint from and the panel
    /// has to look again. The read costs one transcript parse per tick, which
    /// is why it runs only while a working child is actually on screen.
    fn refresh_open_child(&mut self, cx: &mut Context<Self>) {
        let Some(key) = self.mode.detail_key().cloned() else {
            return;
        };
        let Some(pane) = self.target.as_ref().and_then(WeakEntity::upgrade) else {
            return;
        };
        pane.update(cx, |pane, cx| {
            pane.load_background_task_transcript(&key, cx);
        });
    }

    /// Open one child's conversation, remembering the list state so going back
    /// returns to what the user was reading.
    fn open_detail(&mut self, key: BackgroundTaskKey, cx: &mut Context<Self>) {
        let Some(pane) = self.target.as_ref().and_then(WeakEntity::upgrade) else {
            return;
        };
        let (kind, cwd) = {
            let pane = pane.read(cx);
            (pane.agent_kind(), pane.working_directory())
        };
        self.detail_transcript = Some(cx.new(|_| TranscriptView::new(kind, cwd)));
        self.mode
            .open(key.clone(), self.running_expanded, self.finished_expanded);
        // Codex stores a descendant's conversation and hands it over on
        // request; Claude Code has been accumulating it live, so this is a
        // no-op there.
        pane.update(cx, |pane, cx| {
            pane.load_background_task_transcript(&key, cx);
        });
        cx.notify();
    }

    /// Ask the provider to stop one child. A refusal means the child settled
    /// between the snapshot this row was drawn from and the click, so the next
    /// snapshot reports the outcome either way and nothing is said here.
    fn stop_task(&mut self, key: &BackgroundTaskKey, cx: &mut Context<Self>) {
        let Some(pane) = self.target.as_ref().and_then(WeakEntity::upgrade) else {
            return;
        };
        pane.update(cx, |pane, _| pane.interrupt_background_task(key));
    }

    fn close_detail(&mut self, cx: &mut Context<Self>) {
        let Some((running_expanded, finished_expanded)) = self.mode.close() else {
            return;
        };
        self.running_expanded = running_expanded;
        self.finished_expanded = finished_expanded;
        self.detail_transcript = None;
        cx.notify();
    }

    fn render_detail(
        &mut self,
        key: BackgroundTaskKey,
        transcript: Entity<TranscriptView>,
        snapshot: &BackgroundTaskSnapshot,
        now: SystemTime,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(task) = snapshot.tasks.iter().find(|task| task.key == key) else {
            // The child left this session's snapshot, so there is nothing to
            // show; the list is the honest place to be.
            self.close_detail(cx);
            return div().into_any_element();
        };
        // A finished child's conversation cannot change again, so only one
        // that is still working keeps the refresh tick alive.
        let active = task.state.is_active();
        self.sync_elapsed_timer(active, cx);

        let (items, state, dropped, revision) = self
            .target
            .as_ref()
            .and_then(WeakEntity::upgrade)
            .and_then(|pane| {
                let pane = pane.read(cx);
                let child = pane.background_task_transcript(&key)?;
                Some((
                    child.items().to_vec(),
                    child.state().clone(),
                    child.dropped(),
                    child.revision(),
                ))
            })
            .unwrap_or_else(|| (Vec::new(), BackgroundTaskTranscriptState::NotLoaded, 0, 0));

        transcript.update(cx, |view, cx| view.show_items(&items, revision, cx));

        let theme = cx.theme();
        let header = v_flex()
            .px_2()
            .py_1()
            .gap_0p5()
            .border_b_1()
            .border_color(theme.sidebar_border)
            .child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .child(
                        Button::new("background-task-back")
                            .ghost()
                            .xsmall()
                            .icon(IconName::ArrowLeft)
                            .tooltip(i18n("tasks-background-back-tooltip"))
                            .aria_label(i18n("tasks-background-back-tooltip"))
                            .on_click(cx.listener(|this, _, _, cx| this.close_detail(cx))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .truncate()
                            .text_sm()
                            .child(task.display_label()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(state_color(task.state, cx))
                            .child(background_task_state_label(task.state)),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(div().flex_none().child(task.key.provider.label()))
                    .child(div().flex_1().truncate().child(row_detail(task)))
                    .children(row_timing(task, now).map(|timing| div().flex_none().child(timing))),
            );

        let body: AnyElement = match (&state, items.is_empty()) {
            (BackgroundTaskTranscriptState::Unavailable { message }, _) => empty_state(
                i18n("tasks-background-transcript-unavailable-title"),
                &i18n("tasks-background-transcript-unavailable-detail")
                    .replace("{message}", message),
                cx,
            ),
            (BackgroundTaskTranscriptState::Loading, true) => empty_state(
                i18n("tasks-background-loading-title"),
                i18n("tasks-background-transcript-loading-detail"),
                cx,
            ),
            (_, true) => empty_state(
                i18n("tasks-background-transcript-empty-title"),
                i18n("tasks-background-transcript-empty-detail"),
                cx,
            ),
            _ => v_flex()
                .flex_1()
                .min_h_0()
                // The retention bound drops the oldest content, so a truncated
                // conversation says so rather than reading as complete.
                .children((dropped > 0).then(|| {
                    div()
                        .px_2()
                        .py_1()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(
                            i18n("tasks-background-transcript-truncated")
                                .replace("{count}", &dropped.to_string()),
                        )
                }))
                .child(transcript)
                .into_any_element(),
        };

        v_flex()
            .size_full()
            .child(header)
            .child(body)
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
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
    let color = state_color(task.state, cx);
    let name = task.display_label();
    let detail = row_detail(task);
    let timing = row_timing(task, now);
    let state_label = background_task_state_label(task.state);
    // Everything the row shows visually, in one string. A screen reader
    // announces the row as a whole, so it needs the parts the layout separates
    // into two lines plus the child id, which is not rendered anywhere.
    let description: SharedString = format!(
        "{} · {} · {}\n{}",
        task.key.provider.label(),
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
                        .child(div().flex_1().truncate().child(detail))
                        .children(timing.map(|timing| div().flex_none().child(timing))),
                ),
        )
        .children(stop.map(|stop| div().flex_none().child(stop)))
        .into_any_element()
}

/// Objective first, then the latest status line, then a neutral fallback so a
/// row with no optional metadata still reads as a real entry.
fn state_color(state: BackgroundTaskState, cx: &Context<BackgroundTasksView>) -> Hsla {
    let theme = cx.theme();
    match state {
        BackgroundTaskState::Failed => theme.red,
        BackgroundTaskState::NeedsInput => theme.yellow,
        BackgroundTaskState::Done => theme.green,
        BackgroundTaskState::Working | BackgroundTaskState::Starting => theme.foreground,
        BackgroundTaskState::Interrupted | BackgroundTaskState::Stopped => theme.muted_foreground,
    }
}

fn row_detail(task: &BackgroundTaskSummary) -> String {
    task.objective
        .as_deref()
        .or(task.status.as_deref())
        .or(task.last_preview.as_deref())
        .map(str::to_owned)
        .unwrap_or_else(|| i18n("tasks-background-no-description").to_string())
}

fn row_timing(task: &BackgroundTaskSummary, now: SystemTime) -> Option<String> {
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
fn duration_label(now: SystemTime, past: SystemTime) -> String {
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

fn background_task_state_label(state: BackgroundTaskState) -> &'static str {
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
        return Some(i18n("tasks-background-show-fewer").to_string());
    }
    (hidden > 0).then(|| i18n("tasks-background-show-more").replace("{count}", &hidden.to_string()))
}

fn running_heading(snapshot: &BackgroundTaskSnapshot) -> String {
    let active = snapshot.active_count();
    let needs_input = snapshot.needs_input_count();
    if needs_input > 0 {
        return i18n("tasks-background-heading-running-needs-input")
            .replace("{count}", &active.to_string())
            .replace("{needs}", &needs_input.to_string());
    }
    i18n("tasks-background-heading-running").replace("{count}", &active.to_string())
}

fn finished_heading(snapshot: &BackgroundTaskSnapshot) -> String {
    i18n("tasks-background-heading-finished")
        .replace("{count}", &snapshot.terminal_count().to_string())
}

impl Render for BackgroundTasksView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let settings = cx.global::<AppSettings>();
        let font_family = settings.agent_font_family.clone();
        let font_size = px(settings.agent_font_size as f32);

        v_flex()
            .size_full()
            .font_family(font_family)
            .text_size(font_size)
            .children(matches!(self.mode, PanelMode::List).then(|| {
                h_flex()
                    .refine_style(&panel_header(cx))
                    .child(div().text_sm().child(i18n("tasks-background-title")))
            }))
            .child(self.render_body(cx))
    }
}

impl BackgroundTasksView {
    fn render_body(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let snapshot = self.snapshot(cx);
        let now = SystemTime::now();

        // A child's conversation replaces the list; the elapsed-time task is
        // only about the list's running rows, so it stands down while reading.
        if let (Some(key), Some(transcript)) = (
            self.mode.detail_key().cloned(),
            self.detail_transcript.clone(),
        ) {
            self.sync_elapsed_timer(false, cx);
            return match snapshot {
                Some(snapshot) => self.render_detail(key, transcript, &snapshot, now, cx),
                None => {
                    self.close_detail(cx);
                    div().into_any_element()
                }
            };
        }

        let Some(snapshot) = snapshot else {
            self.sync_elapsed_timer(false, cx);
            // An Agent pane publishes its first snapshot only once the adapter
            // has something to report, so a targeted view with none yet is
            // still starting up rather than looking at the wrong kind of tab.
            return match self.target.is_some() {
                true => empty_state(
                    i18n("tasks-background-loading-title"),
                    i18n("tasks-background-loading-detail"),
                    cx,
                ),
                false => empty_state(
                    i18n("tasks-background-no-session-title"),
                    i18n("tasks-background-no-session-detail"),
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
                    i18n("tasks-background-status-unavailable-title"),
                    &i18n("tasks-background-status-unavailable-detail")
                        .replace("{message}", message),
                    cx,
                ),
                BackgroundTaskDiscoveryState::Loading => empty_state(
                    i18n("tasks-background-loading-title"),
                    i18n("tasks-background-loading-detail"),
                    cx,
                ),
                _ => empty_state(
                    i18n("tasks-background-empty-title"),
                    i18n("tasks-background-empty-detail"),
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
