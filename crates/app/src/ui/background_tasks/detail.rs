//! One task opened: its conversation if it has one, its command and output
//! file if it is a shell, and the control that stops it.
//!
//! A child's conversation is not streamed, so opening one asks the pane to
//! read it and the panel redraws when the answer arrives.

use std::time::SystemTime;

use gpui::prelude::*;
use gpui::{AnyElement, Context, Entity, WeakEntity, div};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{ActiveTheme as _, IconName, Sizable as _, h_flex, v_flex};
use nmt_agent_utils::background_task::{
    BackgroundTaskKey, BackgroundTaskSnapshot, BackgroundTaskTranscriptState,
};
use nmt_app_agent::transcript::TranscriptView;
use nmt_i18n::i18n;

use crate::ui::background_tasks::BackgroundTasksView;
use crate::ui::background_tasks::rows::{
    background_task_kind_label, background_task_state_label, empty_state, row_detail, row_timing,
    state_color,
};

impl BackgroundTasksView {
    /// Re-read the open child while it is still working. Claude Code writes a
    /// child's own turns to that child's file instead of publishing them on
    /// the parent stream, so no event arrives to repaint from and the panel
    /// has to look again. The read costs one transcript parse per tick, which
    /// is why it runs only while a working child is actually on screen.
    pub(super) fn refresh_open_child(&mut self, cx: &mut Context<Self>) {
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
    pub(super) fn open_detail(&mut self, key: BackgroundTaskKey, cx: &mut Context<Self>) {
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
    pub(super) fn stop_task(&mut self, key: &BackgroundTaskKey, cx: &mut Context<Self>) {
        let Some(pane) = self.target.as_ref().and_then(WeakEntity::upgrade) else {
            return;
        };
        pane.update(cx, |pane, _| pane.interrupt_background_task(key));
    }

    pub(super) fn close_detail(&mut self, cx: &mut Context<Self>) {
        let Some((running_expanded, finished_expanded)) = self.mode.close() else {
            return;
        };
        self.running_expanded = running_expanded;
        self.finished_expanded = finished_expanded;
        self.detail_transcript = None;
        cx.notify();
    }

    pub(super) fn render_detail(
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
                    .child(
                        div()
                            .flex_none()
                            .child(background_task_kind_label(task.kind)),
                    )
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
}
