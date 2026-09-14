//! The right-hand panel: which of the three views it is showing, and what each
//! is pointed at.
//!
//! Only one can be open at a time, so opening one closes whichever was there,
//! and each is retargeted as the active tab changes rather than rebuilt.

#[cfg(test)]
#[path = "panels_tests.rs"]
mod tests;

use app::agent_tab::AgentPane;
use gpui::{App, Context, Entity};

use crate::ui::git_status::GitStatusModel;
use crate::ui::right_panel::{RightPanel, RightPanelKind};
use crate::ui::shell::Shell;

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

    /// Both views follow the active tab. Unsupported sessions clear their
    /// target while retaining the panel's open state.
    pub(super) fn sync_agent_targets(&self, active: Option<Entity<AgentPane>>, cx: &mut App) {
        let (workflow_target, task_target) = active.map_or((None, None), |pane| {
            let view = pane.read(cx);

            (
                view.workflow_session_id(cx).map(|_| pane.downgrade()),
                view.background_task_parent().map(|_| pane.downgrade()),
            )
        });

        let panel = self.panel.read(cx);
        let workflows = panel.workflows().clone();
        let tasks = panel.tasks().clone();

        workflows.update(cx, |view, cx| view.set_target(workflow_target, cx));
        tasks.update(cx, |view, cx| view.set_target(task_target, cx));
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
