//! The right-hand panel: which of the three views it is showing, and what each
//! is pointed at.
//!
//! Only one can be open at a time, so opening one closes whichever was there,
//! and each is retargeted as the active tab changes rather than rebuilt.

use gpui::{App, Context, Entity, Window};
use nmt_app_agent::AgentPane;
use nmt_config::get;

use crate::ui::git_status::GitStatusModel;
use crate::ui::right_panel::{RightPanel, RightPanelKind};
use crate::ui::shell::Shell;
use crate::ui::shell::actions::{ToggleBackgroundTasks, ToggleGitSidebar, ToggleWorkflows};

/// The right-side area and everything that decides what it shows. The git
/// model sits here because opening or leaving the Git view is what turns its
/// polling on and off, and the two sticky flags because they gate the title-bar
/// controls that open the other two views.
pub(super) struct RightPanelController {
    /// Always mounted so close can animate.
    panel: Entity<RightPanel>,
    /// Shared git status poller feeding the titlebar indicator and sidebar.
    git_model: Entity<GitStatusModel>,
    /// Whether any tab has run a workflow. Sticky: the title-bar control
    /// appears the first time one runs and stays, so a finished run remains
    /// reachable after its rows have settled.
    workflows_seen: bool,
    /// Whether any tab has spawned a background task. Sticky for the same
    /// reason as `workflows_seen`: a child that has finished is still worth
    /// opening the view for.
    background_tasks_seen: bool,
}

impl RightPanelController {
    pub(super) fn new(panel: Entity<RightPanel>, git_model: Entity<GitStatusModel>) -> Self {
        Self {
            panel,
            git_model,
            workflows_seen: false,
            background_tasks_seen: false,
        }
    }

    pub(super) fn panel(&self) -> &Entity<RightPanel> {
        &self.panel
    }

    pub(super) fn git_model(&self) -> &Entity<GitStatusModel> {
        &self.git_model
    }

    pub(super) fn workflows_seen(&self) -> bool {
        self.workflows_seen
    }

    pub(super) fn background_tasks_seen(&self) -> bool {
        self.background_tasks_seen
    }

    pub(super) fn note_workflow_seen(&mut self) {
        self.workflows_seen = true;
    }

    pub(super) fn note_background_task_seen(&mut self, any: bool) {
        self.background_tasks_seen |= any;
    }

    /// Whether the right-side area currently shows this content, which is the
    /// checked state of the title-bar control that opens it.
    pub(super) fn shows(&self, kind: RightPanelKind, cx: &App) -> bool {
        self.panel.read(cx).shows(kind)
    }

    /// Hand the git model the directory to watch; it no-ops when unchanged.
    pub(super) fn set_git_target(&self, cwd: Option<String>, cx: &mut Context<Shell>) {
        self.git_model
            .update(cx, |model, cx| model.set_target_cwd(cwd, cx));
    }

    /// Point the workflow view at the active Agent pane. Only Claude Code
    /// reports workflows, so any other pane clears the target and the view
    /// reports that there is no session rather than closing.
    pub(super) fn sync_workflow_target(
        &self,
        active: Option<Entity<AgentPane>>,
        cx: &mut Context<Shell>,
    ) {
        let handle = active
            .filter(|pane| pane.read(cx).workflow_session_id().is_some())
            .map(|pane| pane.downgrade());
        let workflows = self.panel.read(cx).workflows().clone();
        workflows.update(cx, |view, cx| view.set_target(handle, cx));
    }

    /// Point the view at the active Agent pane. A pane with no supported
    /// provider session clears the target rather than closing the view: the
    /// panel reports that there is nothing to show, which keeps the right-side
    /// area from vanishing while the user moves between tabs.
    pub(super) fn sync_task_target(
        &self,
        active: Option<Entity<AgentPane>>,
        cx: &mut Context<Shell>,
    ) {
        let handle = active
            .filter(|pane| pane.read(cx).background_task_parent().is_some())
            .map(|pane| pane.downgrade());
        let tasks = self.panel.read(cx).tasks().clone();
        tasks.update(cx, |view, cx| view.set_target(handle, cx));
    }

    /// Show `kind`, or close the area when it was already showing. Reports
    /// whether the area ended up open.
    pub(super) fn select(&self, kind: RightPanelKind, cx: &mut Context<Shell>) -> bool {
        self.panel.update(cx, |panel, cx| panel.select(kind, cx))
    }

    /// Match the git poller to whether its own view is on screen. Refreshing on
    /// the open edge keeps a visible sidebar from re-querying every frame.
    pub(super) fn set_git_sidebar_open(&self, open: bool, cx: &mut Context<Shell>) {
        self.git_model.update(cx, |model, cx| {
            model.sidebar_open = open;
            if open {
                model.refresh(cx);
            }
        });
    }
}

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

        self.panels.set_git_target(cwd, cx);
    }

    pub(super) fn on_toggle_git_sidebar(
        &mut self,
        _: &ToggleGitSidebar,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let open = self.panels.select(RightPanelKind::Git, cx);

        self.panels.set_git_sidebar_open(open, cx);

        cx.notify();
    }

    pub(super) fn on_toggle_background_tasks(
        &mut self,
        _: &ToggleBackgroundTasks,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let open = self.panels.select(RightPanelKind::BackgroundTasks, cx);

        if open {
            self.panels.sync_task_target(self.active_agent(), cx);
            // Asking for fresher data happens on the open edge, not on every
            // render, so a visible panel does not re-query the provider each
            // frame. The adapter still ignores overlapping requests.
            if let Some(pane) = self.active_agent() {
                pane.update(cx, |pane, _| pane.refresh_background_tasks());
            }
        }
        // Git content owns the poller's own visibility flag; leaving Git for
        // another view stops the polling it turned on.
        self.panels.set_git_sidebar_open(false, cx);

        cx.notify();
    }

    pub(super) fn on_toggle_workflows(
        &mut self,
        _: &ToggleWorkflows,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let open = self.panels.select(RightPanelKind::Workflows, cx);

        if open {
            self.panels.sync_workflow_target(self.active_agent(), cx);
        }
        // Git owns the poller's own visibility flag; leaving Git for another
        // view stops the polling it turned on.
        self.panels.set_git_sidebar_open(false, cx);

        cx.notify();
    }
}
