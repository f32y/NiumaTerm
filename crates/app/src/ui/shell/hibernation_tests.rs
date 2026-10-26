use std::path::Path;

use gpui::{
    AppContext as _, Context, Entity, Modifiers, Render, TestAppContext, VisualTestContext, Window,
};
use nmt_agent::AgentMonitor;
use nmt_config::Config;
use nmt_config::local_state::TabState;
use nmt_config::system::WarnBeforeTerminatingShell;

use crate::tabs::{TabId, TabManager};
use crate::ui::background_tasks::BackgroundTasksView;
use crate::ui::git_status::GitStatusModel;
use crate::ui::persistence::session_state;
use crate::ui::remote;
use crate::ui::right_panel::RightPanel;
use crate::ui::settings::{AppSettings, SettingsSurface};
use crate::ui::shell::agent_notifications::AgentNotificationState;
use crate::ui::shell::main_surface::tab_surface_view;
use crate::ui::shell::panels::RightPanelController;
use crate::ui::shell::render::ShellChrome;
use crate::ui::shell::updates_layer::UpdateNotificationLayer;
use crate::ui::shell::workspace_dirs::RootAvailability;
use crate::ui::shell::{AppWindow, InlineRenameSession, TabSurface};
use crate::ui::workflows::WorkflowsView;
use crate::ui::workspace_sidebar::Sidebar;
use crate::workspace::{WorkspaceId, WorkspaceManager, WorkspaceRoots};

fn sleeping_shell(cx: &mut VisualTestContext, saved: TabState) -> Entity<AppWindow> {
    cx.update(|window, cx| {
        gpui_component::init(cx);

        let mut config = Config::default();

        config.appearance.show_daily_token_usage = false;
        config.appearance.show_git_status_on_title_bar = false;
        config.agent.show_agent_usage = false;
        config.remote.lan_browse = false;
        config.system.warn_before_terminating_shell = WarnBeforeTerminatingShell::Always;

        cx.set_global(AppSettings::from_config(config));

        remote::initialize(cx);

        cx.new(|cx| {
            let mut tabs = TabManager::new(TabSurface::Settings, TabId(1), "Settings".into());

            tabs.new_tab(
                TabSurface::Hibernated(Box::new(saved)),
                TabId(2),
                "Sleeping".into(),
            );

            let git_model = cx.new(GitStatusModel::new);
            let tasks = cx.new(|_| BackgroundTasksView::new());
            let workflows = cx.new(|_| WorkflowsView::new());
            let panel = cx.new(|_| RightPanel::new(tasks, workflows));

            AppWindow {
                workspaces: WorkspaceManager::new(
                    tabs,
                    WorkspaceId(3),
                    "Workspace".into(),
                    WorkspaceRoots::single(String::new()),
                ),
                remote_workspaces: Default::default(),
                next_id: 4,
                chrome: ShellChrome::new(git_model.clone(), cx),
                agent_notifications: AgentNotificationState::new(AgentMonitor::new(
                    "hibernation-test",
                )),
                window_active: false,
                sidebar: Sidebar::new(220.),
                renames: InlineRenameSession::default(),
                settings: SettingsSurface::default(),
                focus: cx.focus_handle(),
                window_id: window.window_handle().window_id(),
                panels: RightPanelController::new(panel, git_model),
                update_notifications: UpdateNotificationLayer::default(),
                root_availability: RootAvailability::default(),
                doomed_workspace: None,
                settings_close_pending: false,
            }
        })
    })
}

struct SleepingTabProbe(Entity<AppWindow>);

impl Render for SleepingTabProbe {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
        self.0.update(cx, |shell, cx| {
            tab_surface_view(
                shell.workspaces.active_tabs().active(),
                shell.workspaces.active_tabs().list().active_id(),
                None,
                None,
                cx,
            )
        })
    }
}

fn draw_sleeping_tab(
    cx: &mut VisualTestContext,
    shell: &Entity<AppWindow>,
) -> Entity<SleepingTabProbe> {
    let probe =
        cx.update(|window, cx| window.replace_root(cx, |_, _| SleepingTabProbe(shell.clone())));

    cx.refresh().unwrap();

    probe
}

#[gpui::test]
fn switching_to_sleeping_tabs_keeps_the_saved_state_and_close_does_not_start_them(
    cx: &mut TestAppContext,
) {
    let cx = cx.add_empty_window();

    for agent in [None, Some("codex".to_string())] {
        let saved = TabState {
            cwd: Some("C:/sleeping-project".into()),
            agent,
            ..TabState::default()
        };

        let shell = sleeping_shell(cx, saved.clone());

        cx.update(|window, cx| {
            shell.update(cx, |shell, cx| {
                shell.workspaces.active_tabs_mut().list_mut().activate(0);
                shell.show_active_tab(window, cx);
                shell.workspaces.active_tabs_mut().list_mut().activate(1);
                shell.show_active_tab(window, cx);
                shell.ensure_active_tab_live(window, cx);

                if saved.agent.is_none() {
                    assert!(shell.focus_dir_tab(
                        Path::new(saved.cwd.as_ref().unwrap()),
                        window,
                        cx
                    ));
                }

                assert!(shell.try_active_pane().is_none());
                assert!(shell.active_agent().is_none());
                assert!(shell.focus.is_focused(window));

                let snapshot = session_state(&shell.workspaces, None, cx);

                assert_eq!(snapshot.workspaces[0].tabs[1].cwd, saved.cwd);
                assert_eq!(snapshot.workspaces[0].tabs[1].agent, saved.agent);
            });
        });

        let _probe = draw_sleeping_tab(cx, &shell);

        assert!(cx.debug_bounds("wake-hibernated-tab").is_some());

        cx.update(|_, cx| {
            assert!(matches!(
                shell.read(cx).workspaces.active_tabs().active(),
                TabSurface::Hibernated(_)
            ));
        });

        let close = cx.debug_bounds("close-hibernated-tab").unwrap();

        cx.simulate_click(close.center(), Modifiers::default());

        cx.update(|_, cx| {
            let shell = shell.read(cx);

            assert_eq!(shell.workspaces.active_tabs().list().len(), 1);
            assert!(shell.workspaces.active_tabs().active().is_settings());
        });
    }
}

#[cfg(windows)]
#[gpui::test]
fn wake_button_starts_one_terminal_and_can_hibernate_it_again(cx: &mut TestAppContext) {
    use app::terminal_tab::settings::TerminalSettings;

    // A real PTY reports output and resize completion from its I/O thread.
    cx.background_executor.allow_parking();

    let cx = cx.add_empty_window();
    let shell = sleeping_shell(cx, TabState::default());

    cx.update(|_, cx| {
        cx.set_global(TerminalSettings {
            improve_powershell_compatibility: true,
            input_style: Default::default(),
            cursor_shape: Default::default(),
            command_blocks: false,
            smooth_wheel: false,
            scroll_to_bottom_when_typing: true,
            newline_shortcut: Default::default(),
            font_family: "Consolas".into(),
            font_size: 14.,
            line_height: 1.2,
            background_opacity: 1.,
            font_fallbacks: Default::default(),
        });
    });

    let _probe = draw_sleeping_tab(cx, &shell);
    let wake = cx.debug_bounds("wake-hibernated-tab").unwrap();

    cx.simulate_click(wake.center(), Modifiers::default());

    let weak = cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            let pane = shell.try_active_pane().expect("wake starts a terminal");
            let id = pane.entity_id();
            let weak = pane.downgrade();

            drop(pane);

            shell.wake_tab(TabId(2), window, cx);

            assert_eq!(shell.try_active_pane().unwrap().entity_id(), id);

            shell.hibernate_tab_now(TabId(2), window, cx);

            assert!(shell.workspaces.active_tabs().active().is_settings());

            shell.workspaces.active_tabs_mut().list_mut().activate(1);
            shell.show_active_tab(window, cx);

            assert!(matches!(
                shell.workspaces.active_tabs().active(),
                TabSurface::Hibernated(_)
            ));

            weak
        })
    });

    cx.update(|_, _| assert!(weak.upgrade().is_none()));
}
