//! `Background Tasks` view: the work a Codex or Claude Code parent session
//! started and left running — child agents, and the commands Claude Code runs
//! in the background. Provider adapters own that lifecycle, so this component
//! reads the latest snapshot from the active Agent pane and dispatches nothing
//! to a row beyond the one operation a snapshot reports as available — a row
//! offers Stop only while its adapter says that task can be stopped.

mod detail;
pub(super) mod rows;

use std::time::{Duration, SystemTime};

use gpui::prelude::*;
use gpui::{
    AnyElement, Context, Entity, ScrollHandle, SharedString, Task, WeakEntity, Window, div, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{ActiveTheme as _, IconNamed, Sizable as _, StyledExt as _, h_flex, v_flex};
use nmt_agent_utils::background_task::{
    BackgroundTaskDiscoveryState, BackgroundTaskKey, BackgroundTaskSnapshot, BackgroundTaskSummary,
};
use nmt_app_agent::AgentPane;
use nmt_app_agent::transcript::TranscriptView;
use nmt_i18n::i18n;

use crate::ui::AppSettings;
use crate::ui::background_tasks::rows::{
    empty_state, finished_heading, finished_rows, render_row, running_heading, running_rows,
    section_control_label, visible_rows,
};
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

#[cfg(test)]
mod tests;
