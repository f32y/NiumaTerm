//! The right-hand panel: which of the three views it is showing, and what each
//! is pointed at.
//!
//! Only one can be open at a time, so opening one closes whichever was there,
//! and each is retargeted as the active tab changes rather than rebuilt.

use gpui::{App, Context, Window};
use nmt_config::get;

use crate::ui::right_panel::RightPanelKind;
use crate::ui::shell::Shell;
use crate::ui::shell::actions::{ToggleBackgroundTasks, ToggleGitSidebar, ToggleWorkflows};

impl Shell {
    /// Centralized target-CWD sync: read the active pane's
    /// OSC7-tracked CWD (falling back to the configured working-dir) and
    /// hand it to the git model, which no-ops when unchanged. Called on
    /// every render and on `HostEvent::Cwd`, so no switch path is missed.
    pub(super) fn sync_git_target(&self, cx: &mut Context<Self>) {
        // Agent tabs have no OSC7-tracking pane; the configured working dir
        // keeps the git indicator on something sensible.
        let cwd = self
            .try_active_pane()
            .and_then(|pane| pane.read(cx).tab_state().cwd)
            .or_else(|| get().working_dir.clone());

        self.git_model
            .update(cx, |model, cx| model.set_target_cwd(cwd, cx));
    }

    pub(super) fn on_toggle_git_sidebar(
        &mut self,
        _: &ToggleGitSidebar,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let open = self
            .right_panel
            .update(cx, |panel, cx| panel.select(RightPanelKind::Git, cx));

        self.git_model.update(cx, |model, cx| {
            model.sidebar_open = open;
            if open {
                model.refresh(cx);
            }
        });

        cx.notify();
    }

    pub(super) fn on_toggle_background_tasks(
        &mut self,
        _: &ToggleBackgroundTasks,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let open = self.right_panel.update(cx, |panel, cx| {
            panel.select(RightPanelKind::BackgroundTasks, cx)
        });

        if open {
            self.sync_task_panel_target(cx);
            // Asking for fresher data happens on the open edge, not on every
            // render, so a visible panel does not re-query the provider each
            // frame. The adapter still ignores overlapping requests.
            if let Some(pane) = self.active_agent() {
                pane.update(cx, |pane, _| pane.refresh_background_tasks());
            }
        }
        // Git content owns the poller's own visibility flag; leaving Git for
        // another view stops the polling it turned on.
        self.git_model
            .update(cx, |model, _| model.sidebar_open = false);

        cx.notify();
    }

    pub(super) fn workflows_seen(&self) -> bool {
        self.workflows_seen
    }

    pub(super) fn background_tasks_seen(&self) -> bool {
        self.background_tasks_seen
    }

    /// Whether the right-side area currently shows this content, which is the
    /// checked state of the title-bar control that opens it.
    pub(super) fn right_panel_shows(&self, kind: RightPanelKind, cx: &App) -> bool {
        self.right_panel.read(cx).shows(kind)
    }

    pub(super) fn on_toggle_workflows(
        &mut self,
        _: &ToggleWorkflows,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let open = self
            .right_panel
            .update(cx, |panel, cx| panel.select(RightPanelKind::Workflows, cx));

        if open {
            self.sync_workflow_panel_target(cx);
        }
        // Git owns the poller's own visibility flag; leaving Git for another
        // view stops the polling it turned on.
        self.git_model
            .update(cx, |model, _| model.sidebar_open = false);

        cx.notify();
    }

    /// Point the workflow view at the active Agent pane. Only Claude Code
    /// reports workflows, so any other pane clears the target and the view
    /// reports that there is no session rather than closing.
    pub(super) fn sync_workflow_panel_target(&mut self, cx: &mut Context<Self>) {
        let target = self
            .active_agent()
            .filter(|pane| pane.read(cx).workflow_session_id().is_some());

        let handle = target.map(|pane| pane.downgrade());
        let workflows = self.right_panel.read(cx).workflows().clone();
        workflows.update(cx, |view, cx| view.set_target(handle, cx));
    }

    /// Point the view at the active Agent pane. A pane with no supported
    /// provider session clears the target rather than closing the view: the
    /// panel reports that there is nothing to show, which keeps the right-side
    /// area from vanishing while the user moves between tabs.
    pub(super) fn sync_task_panel_target(&mut self, cx: &mut Context<Self>) {
        let target = self
            .active_agent()
            .filter(|pane| pane.read(cx).background_task_parent().is_some());

        let handle = target.map(|pane| pane.downgrade());
        let tasks = self.right_panel.read(cx).tasks().clone();
        tasks.update(cx, |view, cx| view.set_target(handle, cx));
    }
}
