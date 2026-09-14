use std::cell::Cell;
use std::rc::Rc;

use app::agent_tab::execution::AgentSession;
use app::agent_tab::settings::AgentSettings;
use app::agent_tab::{AgentPane, AgentThreadDefaults};
use gpui::{AppContext as _, TestAppContext};
use nmt_agent::AgentWorkspace;
use nmt_config::profile::AgentProfile;

use crate::ui::AppSettings;
use crate::ui::background_tasks::BackgroundTasksView;
use crate::ui::git_sidebar::GitSidebar;
use crate::ui::git_status::GitStatusModel;
use crate::ui::right_panel::RightPanel;
use crate::ui::shell::panels::RightPanelController;
use crate::ui::workflows::WorkflowsView;

#[gpui::test]
fn leaving_an_agent_tab_clears_both_panel_targets(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);

        cx.set_global(AppSettings::default());

        cx.set_global(AgentSettings::default());

        cx.set_global(AgentThreadDefaults::default());
    });

    let cx = cx.add_empty_window();

    let (_owner, _pane, controller, workflows, tasks) = cx.update(|window, cx| {
        let owner =
            AgentSession::create(AgentProfile::default(), AgentWorkspace::default(), None, cx);

        let pane = cx.new(|cx| AgentPane::attach(&owner, window, cx));
        let workflows = cx.new(|_| WorkflowsView::new());
        let tasks = cx.new(|_| BackgroundTasksView::new());

        workflows.update(cx, |view, cx| view.set_target(Some(pane.downgrade()), cx));

        tasks.update(cx, |view, cx| view.set_target(Some(pane.downgrade()), cx));

        let git_model = cx.new(GitStatusModel::new);
        let git = cx.new(|cx| GitSidebar::new(git_model.clone(), cx));
        let panel = cx.new(|_| RightPanel::new(git, tasks.clone(), workflows.clone()));

        (
            owner,
            pane,
            RightPanelController::new(panel, git_model),
            workflows,
            tasks,
        )
    });

    let changes = Rc::new(Cell::new(0));
    let workflow_changes = changes.clone();
    let task_changes = changes.clone();

    let _subscriptions = cx.update(|_, cx| {
        (
            cx.observe(&workflows, move |_, _| {
                workflow_changes.set(workflow_changes.get() + 1)
            }),
            cx.observe(&tasks, move |_, _| task_changes.set(task_changes.get() + 1)),
        )
    });

    cx.update(|_, cx| controller.sync_agent_targets(None, cx));

    cx.run_until_parked();

    assert_eq!(changes.get(), 2);

    cx.update(|_, cx| controller.sync_agent_targets(None, cx));

    cx.run_until_parked();

    assert_eq!(changes.get(), 2);
}
