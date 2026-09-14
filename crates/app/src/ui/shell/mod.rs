#[cfg(windows)]
pub(crate) use crate::ui::shell::actions::NewRemoteTab;

pub(crate) use crate::ui::shell::actions::{
    CloseTab, NewAgentTab, NewTab, NewWindow, NewWorkspace, NextTab, NextWorkspace, PrevTab,
    PrevWorkspace, ResizePaneDown, ResizePaneLeft, ResizePaneRight, ResizePaneUp, ShowSettings,
    SplitDown, SplitLeft, SplitRight, SplitUp, ToggleBackgroundTasks, ToggleGitSidebar,
    ToggleSidebar, ToggleWorkflows,
};

pub(crate) use crate::ui::shell::tab_surface::TabSurface;

pub(super) use crate::ui::shell::inline_rename::{InlineRename, InlineRenameStyle};

pub(super) use crate::ui::shell::rename::InlineRenameSession;

pub(super) use crate::ui::shell::tab_presentation::pending_tab_icon;

pub(crate) mod tab_surface;

mod actions;

mod agent_notifications;

mod inline_rename;

mod panels;

mod rename;

mod render;

mod settings_workspace;

mod tab_presentation;

mod updates_layer;

mod workspace_dirs;

#[cfg(test)]
mod tests;

use std::borrow::Cow;

use std::path::PathBuf;

use std::rc::Rc;

use std::{collections, io, iter, path, thread, time};

use app::agent_tab::execution::AgentSession;

use app::agent_tab::team::{TeamPane, TeamRuntime};

use app::agent_tab::{AgentPane, AgentPaneEvent, RecoveryIdentity};

use app::terminal_tab::session::HostEvent;

use app::terminal_tab::view::{AgentInterrupted, TerminalGridResized, TerminalPane};

use dirs::home_dir;

use gpui::prelude::*;

use gpui::{
    Anchor, AnyElement, App, Axis, Context, Div, Entity, FocusHandle, Focusable, KeyDownEvent,
    MouseDownEvent, ObjectFit, Pixels, Render, SharedString, Window, WindowBounds, WindowId, div,
    img, px, relative,
};

use gpui_component::button::{Button, ButtonVariants, Toggle, ToggleVariants};

use gpui_component::dialog::{
    DIALOG_BUTTON_MIN_WIDTH, Dialog, DialogAction, DialogButtonProps, DialogClose, DialogFooter,
};

use gpui_component::input::{Input, InputState};

use gpui_component::modern_menu::{ModernMenu, dispatch_modern_menu_key};

use gpui_component::notification::{Notification, NotificationType};

use gpui_component::progress::Progress;

use gpui_component::resizable::{PANEL_MIN_SIZE, ResizablePanelGroup, resizable_panel};

use gpui_component::{
    ActiveTheme, Disableable, Icon, IconName, IconNamed, Root, StyledExt, TitleBar, WindowExt,
    h_flex, v_flex,
};

use nmt_agent::team::identity::RoomId;

use nmt_agent::update::{ProviderKind, UpdatePhase};

use nmt_agent::{
    AgentActivityPolicy, AgentEvent, AgentMonitor, AgentNotification, AgentRoute,
    AgentRuntimeStatus, AgentWorkspace, MonitorMutation, agent_process, request_native_delivery,
};

use nmt_config::local_state::{TabState, WindowState};

use nmt_config::system::WarnBeforeTerminatingShell;

use nmt_config::{config_dir_path, get};

use nmt_platform::window::native_active_state;

use nmt_platform::{
    NativeNotification, remove_notification, show_notification, system_notification_enabled,
};

use rust_i18n::t;

use tracing::warn;

use crate::agent_updates::{
    AgentUpdates, FocusedVisibleLifetime, NotificationPrimaryAction, NotificationProgress,
    UpdateNotificationTone, UpdateNotificationView,
};

use crate::agent_usage::AgentUsageView;

use crate::cli::CliAction;

use crate::pane_tree::{PaneId, PaneNode, SplitDirection};

#[cfg(windows)]
use crate::remote;

use crate::tabs::{Tab, TabId, TabManager};

use crate::ui::background_tasks::BackgroundTasksView;

use crate::ui::composition::FLOATING_SURFACE_SIDE_INSET;

use crate::ui::git_sidebar::GitSidebar;

use crate::ui::git_status::{GitStatusModel, GitStatusView};

use crate::ui::persistence::{
    default_session, materialize_active_tab, restore_session, session_state, spawn_default_pane,
};

use crate::ui::right_panel::{RightPanel, RightPanelKind};

use crate::ui::settings::{AgentProfile, AppSettings, TabBarStyle};

use crate::ui::shell::actions::NewTeamTab;

use crate::ui::shell::agent_notifications::AgentNotificationState;

use crate::ui::shell::panels::RightPanelController;

use crate::ui::shell::render::ShellChrome;

use crate::ui::shell::settings_workspace::{SettingsSurface, settings_title};

use crate::ui::shell::tab_surface::AgentTab;

use crate::ui::shell::updates_layer::UpdateNotificationLayer;

use crate::ui::shell::workspace_dirs::{RootAvailability, WorkspaceDirsEditor};

use crate::ui::tab_bar::TabStrip;

use crate::ui::terminal_launch::attach_remote;

use crate::ui::terminal_layout::TerminalLayout;

use crate::ui::token_usage::TokenUsageView;

use crate::ui::workflows::WorkflowsView;

use crate::ui::workspace_sidebar::{Sidebar, SidebarTab, SidebarUsage, WorkspaceChrome};

use crate::ui::{UI_RADIUS, main_view_background_opacity, workspace_sidebar};

#[cfg(windows)]
use crate::update::check;

use crate::usage_sources::daily_source;

use crate::window::{AppWindow, LastActiveWindow, ShellEntry, ShellRegistry, WindowRegistry};

use crate::workspace::{
    ProgressTally, TerminalActivity, WorkspaceId, WorkspaceKind, WorkspaceManager, WorkspaceRoots,
    best_match, exact_match,
};

use crate::{agent_updates, ui};

/// A workspace cwd as a shell working directory: `None` for empty or the
/// legacy `"."` placeholder (shells then start in their default directory).
pub(super) fn explicit_cwd(cwd: &str) -> Option<String> {
    let cwd = cwd.trim();

    (!cwd.is_empty() && cwd != ".").then(|| cwd.to_string())
}

/// The directory list an Agent Tab of `roots` starts with. Placeholder entries
/// are dropped for the same reason [`explicit_cwd`] drops them: they name no
/// directory a harness could be pointed at.
pub(super) fn agent_workspace(roots: Option<&WorkspaceRoots>) -> AgentWorkspace {
    let Some(roots) = roots else {
        return AgentWorkspace::default();
    };

    AgentWorkspace::new(
        explicit_cwd(roots.primary()),
        roots
            .additional()
            .iter()
            .filter_map(|path| explicit_cwd(path))
            .collect(),
    )
}

/// A conversation to reopen in a tab rooted where it ran, carrying the profile
/// of the tab that listed it so the new tab launches the same agent.
pub(super) struct PendingAgentResume {
    pub(super) profile: AgentProfile,
    pub(super) cwd: String,
    pub(super) session_id: String,
}

pub(crate) struct Shell {
    pub(crate) workspaces: WorkspaceManager,

    /// Monotonic surface-id source shared by tabs and workspaces.
    next_id: u64,

    chrome: ShellChrome,
    agent_notifications: AgentNotificationState,

    window_active: bool,

    /// Workspace-sidebar view state (collapse/expand + width) and its renderer.
    pub(super) sidebar: Sidebar,

    /// In-flight inline renames: a sidebar item or a tab renders an input in
    /// place of its name. Enter or clicking anywhere else (blur) commits.
    pub(crate) renames: InlineRenameSession,

    settings: SettingsSurface,
    focus: FocusHandle,

    /// This shell's window in the `WindowRegistry`; all state writes target
    /// this entry.
    pub(crate) window_id: WindowId,

    /// The right-side area and what points it at the active tab.
    panels: RightPanelController,

    /// Stable entities let each installation's card replace content in place
    /// without entering the transient Root notification lifecycle.
    /// On-screen provider-update notifications by key. Card entity, source
    /// view, and auto-hide clock live in one record so retiring a key cannot
    /// leave a stale sibling behind.
    update_notifications: UpdateNotificationLayer,

    root_availability: RootAvailability,

    /// Workspace excluded from session persistence: the user chose Quit in
    /// the close-last-workspace dialog, so it must not be restored on the
    /// next launch. Only set on the quit path — cancelling keeps everything.
    pub(crate) doomed_workspace: Option<WorkspaceId>,
}

impl Drop for Shell {
    fn drop(&mut self) {
        Self::remove_native_notifications(&self.agent_notifications.agent_monitor.notifications());
    }
}

impl Shell {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe_global_in::<AppSettings>(window, |this, window, cx| {
            this.sync_team_setting(window, cx);

            cx.notify();
        })
        .detach();

        cx.observe_global::<AgentUpdates>(|_, cx| cx.notify())
            .detach();

        // Stash the window geometry on every move/resize; main.rs flushes it
        // to local_state.toml on quit. Fires for both, and the Maximized
        // variant carries the restore bounds. Scan-and-update only: a stale
        // event after the window's entry is removed is a no-op.
        let window_id = window.window_handle().window_id();

        cx.observe_window_bounds(window, Self::on_window_bounds_changed)
            .detach();

        // Expose this shell to the CLI dispatch task and track which window
        // was focused last (the `new_tab`/`activate` URL target).
        let entry = ShellEntry {
            window_id,
            handle: window.window_handle(),
            shell: cx.weak_entity(),
        };

        cx.global_mut::<ShellRegistry>().0.push(entry);

        // OS-level close requests (Alt+F4, taskbar, system menu) go through
        // the running-processes confirmation. The titlebar X bypasses
        // WM_CLOSE, so it routes through the same check via `on_close_window`.
        let weak = cx.weak_entity();

        window.on_window_should_close(cx, move |window, cx| {
            weak.update(cx, |this, cx| this.confirm_window_close(window, cx))
                .unwrap_or(true)
        });

        cx.observe_window_activation(window, Self::on_window_activation)
            .detach();

        let default_profile = cx.global::<AppSettings>().default_profile_command();
        let registry_entry = cx.global::<WindowRegistry>().get(window_id);

        // A CLI new_window target replaces session restore for this window.
        let initial_cwd = registry_entry.and_then(|entry| entry.initial_cwd.clone());

        let remembered_session = if initial_cwd.is_some() {
            None
        } else {
            registry_entry.and_then(|entry| entry.session.clone())
        };

        let sidebar_width = registry_entry
            .and_then(|entry| entry.sidebar_width)
            .map(|width| width.clamp(workspace_sidebar::MIN_WIDTH, workspace_sidebar::MAX_WIDTH))
            .unwrap_or(workspace_sidebar::SIDEBAR_WIDTH.max(workspace_sidebar::MIN_WIDTH));

        let mut restore_next_id = 1;

        let restored = restore_session(remembered_session, &mut restore_next_id, window, cx);

        let (workspaces, next_id) = if let Some(workspaces) = restored {
            (workspaces, restore_next_id)
        } else {
            let mut next_id = 1;

            let workspaces = default_session(initial_cwd, default_profile, &mut next_id, cx);

            (workspaces, next_id)
        };

        let now = time::Instant::now();

        let mut agent_monitor = AgentMonitor::new(agent_process().process_instance());

        for tabs in workspaces.all_tabs() {
            for tab in tabs.list().items() {
                let activity_policy = match tab.surface() {
                    TabSurface::Agent(_) => AgentActivityPolicy::ExplicitLifecycle,
                    _ => AgentActivityPolicy::ExpireAfterInactivity,
                };

                for route in Self::agent_routes_in_surface(tab.surface(), cx) {
                    agent_monitor.register_route(route, activity_policy, now);
                }
            }
        }

        let git_model = cx.new(GitStatusModel::new);

        let mut this = Self {
            workspaces,
            next_id,
            chrome: ShellChrome::new(git_model.clone(), cx),
            agent_notifications: AgentNotificationState::new(agent_monitor),
            window_active: Self::exact_window_active(window),
            sidebar: Sidebar::new(sidebar_width),
            renames: InlineRenameSession::default(),
            settings: SettingsSurface::default(),
            focus: cx.focus_handle(),
            window_id,
            panels: {
                let git = cx.new(|cx| GitSidebar::new(git_model.clone(), cx));
                let tasks = cx.new(|_| BackgroundTasksView::new());
                let workflows = cx.new(|_| WorkflowsView::new());
                let panel = cx.new(|_| RightPanel::new(git, tasks, workflows));

                RightPanelController::new(panel, git_model)
            },
            update_notifications: UpdateNotificationLayer::default(),
            root_availability: RootAvailability::default(),
            doomed_workspace: None,
        };

        this.sync_session_memory(cx);

        this.refresh_root_availability(cx);

        this
    }

    fn on_window_bounds_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = window.window_handle().window_id();
        let window_bounds = window.window_bounds();
        let bounds = window_bounds.get_bounds();

        if let Some(entry) = cx.global_mut::<WindowRegistry>().get_mut(id) {
            entry.bounds = Some(WindowState {
                x: bounds.origin.x.as_f32(),
                y: bounds.origin.y.as_f32(),
                width: bounds.size.width.as_f32(),
                height: bounds.size.height.as_f32(),
                maximized: matches!(window_bounds, WindowBounds::Maximized(_)),
            });
        }
    }

    fn on_window_activation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.window_active = Self::exact_window_active(window);

        if self.window_active {
            cx.global_mut::<LastActiveWindow>().0 = Some(self.window_id);

            self.acknowledge_visible(window, true, cx);
        } else {
            // A context menu drawn in its own window never takes activation,
            // so it has none of its own to lose. This window losing it is
            // what says the user has moved on from the menu.
            ui::dismiss_modern_menu(cx);
        }

        self.process_native_notifications(cx);

        cx.notify();
    }

    pub(crate) fn alloc_id(next_id: &mut u64) -> u64 {
        let id = *next_id;

        *next_id += 1;

        id
    }

    /// The active tab's focused terminal pane entity.
    pub(crate) fn active_pane(&self) -> Entity<TerminalPane> {
        self.workspaces
            .active_tabs()
            .active()
            .live()
            .tree()
            .focused_pane()
            .clone()
    }

    /// The focused terminal pane, or `None` when the active tab has no
    /// terminal (an agent tab). Terminal-only funnels that also run while an
    /// agent tab is active must go through this instead of `active_pane`.
    fn try_active_pane(&self) -> Option<Entity<TerminalPane>> {
        self.workspaces
            .active_tabs()
            .active()
            .tree()
            .map(|tree| tree.tree().focused_pane().clone())
    }

    /// The active tab's agent view, when it is an agent tab.
    fn active_agent(&self) -> Option<Entity<AgentPane>> {
        self.workspaces.active_tabs().active().agent().cloned()
    }

    fn active_agent_route(&self, cx: &App) -> Option<AgentRoute> {
        self.active_agent()
            .and_then(|pane| pane.read(cx).agent_route(cx).cloned())
            .or_else(|| {
                self.try_active_pane()
                    .map(|pane| pane.read(cx).agent_route().clone())
            })
    }

    /// Spawn a still-pending (lazily-restored) active tab, then register its
    /// panes' agent routes — the startup registration sweep only saw tabs that
    /// were live at window creation.
    fn ensure_active_tab_live(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !materialize_active_tab(&mut self.workspaces, &mut self.next_id, window, cx) {
            return;
        }

        let surface = self.workspaces.active_tabs().active();

        let activity_policy = match surface {
            TabSurface::Agent(_) => AgentActivityPolicy::ExplicitLifecycle,
            _ => AgentActivityPolicy::ExpireAfterInactivity,
        };

        let routes = Self::agent_routes_in_surface(surface, cx);

        let now = time::Instant::now();

        for route in routes {
            self.agent_notifications
                .agent_monitor
                .register_route(route, activity_policy, now);
        }
    }

    fn sync_team_setting(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if cx.global::<AppSettings>().config().agent.enable_agent_team {
            if matches!(
                self.workspaces.active_tabs().active(),
                TabSurface::TeamDisabled(_)
            ) {
                self.on_active_tab_changed(window, cx);
            }

            return;
        }

        let teams: Vec<_> = self
            .workspaces
            .all_tabs()
            .flat_map(|tabs| tabs.list().items())
            .filter(|tab| tab.surface().team().is_some())
            .map(Tab::id)
            .collect();

        for id in teams {
            if let Some(tab) = self
                .workspaces
                .tabs_for_tab_mut(id)
                .and_then(|tabs| tabs.list_mut().find_mut(id))
            {
                tab.surface_mut().disable_team(cx);
            }
        }
    }

    fn sync_active_terminal_title(&mut self, cx: &App) {
        let Some(pane) = self.try_active_pane() else {
            return;
        };

        let title = pane.read(cx).terminal_title();
        let tabs = self.workspaces.active_tabs_mut();
        let tab_id = tabs.list().active_id();

        tabs.set_title(tab_id, title);
    }

    pub(crate) fn on_active_tab_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_active_tab_live(window, cx);

        self.sync_active_terminal_title(cx);

        let tabs = self.workspaces.active_tabs_mut();

        if tabs.clear_active_bell() | tabs.clear_active_outcome() {
            cx.notify();
        }

        self.acknowledge_visible(window, true, cx);
    }

    pub(crate) fn focus_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Settings owns its controls' focus and has no pane to focus.
        if self.workspaces.active_tabs().active().is_settings() {
            window.focus(&self.focus, cx);

            return;
        }

        if let Some(team) = self.workspaces.active_tabs().active().team().cloned() {
            team.update(cx, |pane, cx| pane.focus(window, cx));

            return;
        }

        if matches!(
            self.workspaces.active_tabs().active(),
            TabSurface::TeamUnavailable { .. } | TabSurface::TeamDisabled(_)
        ) {
            return;
        }

        if let Some(agent) = self.active_agent() {
            agent.update(cx, |pane, cx| pane.focus(window, cx));

            return;
        }

        let handle = self.active_pane().read(cx).focus.clone();

        window.focus(&handle, cx);
    }

    fn acknowledge_visible(
        &mut self,
        window: &Window,
        include_native_delivered: bool,
        cx: &mut Context<Self>,
    ) {
        if !Self::exact_window_active(window) {
            return;
        }

        let Some(route) = self.active_agent_route(cx) else {
            return;
        };

        let Some(id) = self
            .agent_notifications
            .agent_monitor
            .notification(&route)
            .filter(|notification| {
                !notification.read && (include_native_delivered || !notification.native_requested)
            })
            .map(|notification| notification.id.clone())
        else {
            return;
        };

        self.acknowledge_notification(&route, &id, cx);
    }

    /// One tab's terminal activity: a live command outranks the tab's recorded
    /// outcome, because that recorded result belongs to a command that already
    /// ended and a new one is running in the same place.
    pub(super) fn tab_terminal_activity(tab: &Tab<TabSurface>, cx: &App) -> TerminalActivity {
        if tab
            .surface()
            .leaves()
            .into_iter()
            .any(|(_, pane)| pane.read(cx).command_running())
        {
            return TerminalActivity::Running;
        }

        tab.last_outcome()
            .map_or(TerminalActivity::Idle, TerminalActivity::Finished)
    }

    fn workspace_chrome(&self, cx: &App) -> Vec<WorkspaceChrome> {
        self.workspaces
            .summaries()
            .into_iter()
            .map(|summary| {
                let tabs = self.workspaces.tabs_of(summary.id);

                let routes: Vec<_> = tabs
                    .into_iter()
                    .flat_map(|tabs| tabs.list().items())
                    .flat_map(|tab| Self::agent_routes_in_surface(tab.surface(), cx))
                    .collect();

                let agent = self.agent_notifications.agent_monitor.project(&routes);

                let terminal_activity = tabs
                    .into_iter()
                    .flat_map(|tabs| tabs.list().items())
                    .map(|tab| Self::tab_terminal_activity(tab, cx))
                    .fold(TerminalActivity::Idle, TerminalActivity::merge);

                let progress = tabs
                    .into_iter()
                    .flat_map(|tabs| tabs.list().items())
                    .filter_map(|tab| tab.surface().agent())
                    .filter_map(|pane| pane.read(cx).task_tally(cx))
                    .map(|(done, total)| ProgressTally::tasks(done, total))
                    .fold(summary.terminal_progress, ProgressTally::merge);

                WorkspaceChrome {
                    summary,
                    agent,
                    terminal_activity,
                    progress,
                }
            })
            .collect()
    }

    /// Project each tab's routes once for the two chrome indicators. Busy is
    /// limited to the dedicated Agent surface: a terminal tab reports its own
    /// activity through [`Self::tab_terminal_activity`], which is driven by
    /// OSC 133 rather than by an agent route.
    ///
    /// Every workspace takes part, not just the active one: the vertical
    /// tab-bar style shows every workspace's tabs at once. Tab ids are unique
    /// across workspaces, so the wider sets answer the same lookups.
    fn tab_agent_indicators(
        &self,
        cx: &App,
    ) -> (collections::HashSet<TabId>, collections::HashSet<TabId>) {
        let mut unread_tabs = collections::HashSet::new();
        let mut busy_agent_tabs = collections::HashSet::new();

        for tab in self
            .workspaces
            .all_tabs()
            .flat_map(|tabs| tabs.list().items())
        {
            let routes = Self::agent_routes_in_surface(tab.surface(), cx);
            let projection = self.agent_notifications.agent_monitor.project(&routes);

            if projection.unread_count > 0 {
                unread_tabs.insert(tab.id());
            }

            if matches!(tab.surface(), TabSurface::Agent(_))
                && projection.status == AgentRuntimeStatus::Running
            {
                busy_agent_tabs.insert(tab.id());
            }
        }

        (unread_tabs, busy_agent_tabs)
    }

    /// Active tab's display title with the `[exited]` suffix, for the window title.
    fn active_tab_title(&self) -> String {
        let tabs = self.workspaces.active_tabs();
        let tab = &tabs.list().items()[tabs.list().active_index()];

        let base = if tab.title().is_empty() {
            "PowerShell"
        } else {
            tab.title()
        };

        if tab.exited() {
            t!("shell-tab-exited-title", title = base).into_owned()
        } else {
            base.to_string()
        }
    }

    fn on_toggle_sidebar(
        &mut self,
        _: &ToggleSidebar,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sidebar.collapsed = !self.sidebar.collapsed;
        self.sidebar.animated = true;

        cx.notify();
    }

    pub(super) fn on_close_tab(
        &mut self,
        _: &CloseTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A tab without panes (agent tab) has no pane-close cascade; it goes
        // straight to the tab-close path.
        let Some(tree) = self.workspaces.active_tabs().active().tree() else {
            let id = self.workspaces.active_tabs().list().active_id();

            self.request_close_tab(id, window, cx);

            return;
        };

        // With the tab split, the close shortcut closes the focused pane; the
        // last remaining pane falls through to the tab-close cascade.
        if !tree.tree().is_single_leaf() {
            self.request_close_pane(window, cx);

            return;
        }

        let id = self.workspaces.active_tabs().list().active_id();

        self.request_close_tab(id, window, cx);
    }

    /// Close the focused pane of the active (multi-pane) tab, with a confirm
    /// dialog first when its shell has running child processes.
    fn request_close_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = self
            .workspaces
            .active_tabs()
            .active()
            .live()
            .tree()
            .focused();

        let pane = self.active_pane();
        let settings = cx.global::<AppSettings>();

        let count = if settings.config().system.manage_subprocess_job
            && settings.config().system.warn_before_terminating_shell
                != WarnBeforeTerminatingShell::Disabled
        {
            pane.read(cx).child_process_count()
        } else {
            Ok(0)
        };

        if !should_confirm_close(
            false,
            settings.config().system.warn_before_terminating_shell,
            &count,
        ) {
            self.close_pane_now(id, window, cx);

            return;
        }

        let description = Self::close_description(
            count,
            "shell-close-pane-description",
            "shell-close-pane-processes-description",
        );

        Self::open_close_confirm(
            window,
            cx,
            t!("shell-close-pane-title"),
            description,
            None,
            move |this, window, cx| this.close_pane_now(id, window, cx),
        );
    }

    fn close_pane_now(&mut self, id: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        let tree = self.workspaces.active_tabs_mut().active_mut().live_mut();

        let Some(pane) = tree.remove(id, cx) else {
            return;
        };

        let route = pane.read(cx).agent_route().clone();

        self.remove_agent_route(&route, cx);

        // Dropping the pane entity drops its surface, releasing the IO thread
        // and ConPTY handle (same Drop chain as a tab close).
        drop(pane);

        self.on_active_tab_changed(window, cx);

        self.focus_active(window, cx);

        self.sync_session_memory(cx);

        cx.notify();
    }

    fn close_description(count: io::Result<usize>, plain: &str, with_processes: &str) -> String {
        match count {
            Ok(count) if count > 0 => {
                t!(with_processes, processes = &Self::processes_running(count)).into_owned()
            }
            Ok(_) => t!(plain).into_owned(),
            Err(error) => {
                warn!("failed to count processes before closing: {error}");

                t!(plain).into_owned()
            }
        }
    }

    /// "1 child process is running" / "N child processes are running" — the
    /// lead-in of every close-confirmation description.
    fn processes_running(count: usize) -> String {
        if count == 1 {
            t!("shell-close-one-process-running").to_string()
        } else {
            t!("shell-close-many-processes-running", count = count).into_owned()
        }
    }

    /// "You have N temporary workspaces." for the dialogs that end this
    /// window, or `None` when every workspace here is saved. Temporary
    /// workspaces stay out of local_state, so they are the part of the window
    /// that will not come back.
    fn temporary_workspace_note(&self) -> Option<SharedString> {
        let count = self
            .workspaces
            .summaries()
            .iter()
            .filter(|ws| ws.temporary)
            .count();

        match count {
            0 => None,
            1 => Some(t!("shell-close-one-temporary-workspace").into()),
            _ => Some(
                t!("shell-close-many-temporary-workspaces", count = count)
                    .into_owned()
                    .into(),
            ),
        }
    }

    /// Child processes running across every pane of workspace `id`.
    fn workspace_process_count(&self, id: WorkspaceId, cx: &App) -> io::Result<usize> {
        self.workspaces.tabs_of(id).map_or(Ok(0), |tabs| {
            tabs.list()
                .items()
                .iter()
                .map(|tab| self.close_process_count(tab.surface(), cx))
                .sum()
        })
    }

    /// Shared scaffolding of every close-confirmation alert: title +
    /// description, OK runs `on_confirm` against this shell. `note` adds a
    /// bold line under the description for a consequence the description
    /// itself does not cover.
    fn open_close_confirm(
        window: &mut Window,
        cx: &mut Context<Self>,
        // Dialog callbacks can rebuild their content, so they retain a
        // translated title that can be reused on each invocation.
        title: Cow<'static, str>,
        description: String,
        note: Option<SharedString>,
        on_confirm: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let shell = cx.entity();
        let on_confirm = Rc::new(on_confirm);

        window.open_alert_dialog(cx, move |alert, _, _| {
            let shell = shell.clone();
            let on_confirm = Rc::clone(&on_confirm);

            alert
                .confirm()
                .title(title.clone())
                .description(
                    v_flex()
                        .gap_1()
                        .child(description.clone())
                        .children(note.clone().map(|note| div().font_bold().child(note))),
                )
                .on_ok(move |_, window, cx| {
                    let on_confirm = Rc::clone(&on_confirm);

                    shell.update(cx, |this, cx| on_confirm(this, window, cx));

                    true
                })
        });
    }

    /// Child processes running across every pane of this tab, summed over
    /// each shell's Job Object. The count enriches warnings but is not needed
    /// by the `Always` mode.
    fn close_process_count(&self, tree: &TabSurface, cx: &App) -> io::Result<usize> {
        let settings = cx.global::<AppSettings>();

        if !settings.config().system.manage_subprocess_job
            || settings.config().system.warn_before_terminating_shell
                == WarnBeforeTerminatingShell::Disabled
        {
            return Ok(0);
        }

        tree.leaves()
            .into_iter()
            .map(|(_, pane)| pane.read(cx).child_process_count())
            .sum()
    }

    /// Close a tab, with a confirm dialog first when the shell has running
    /// child processes. Closing the last tab closes its workspace too
    /// (confirmed first); on the last workspace that routes into the
    /// quit/replace/cancel dialog.
    ///
    /// The tab need not be in the active workspace: the sidebar lists every
    /// workspace's tabs, and closing one there leaves the user where they
    /// are. Every lookup is therefore keyed on the workspace that holds the
    /// tab rather than on the active one.
    pub(crate) fn request_close_tab(
        &mut self,
        id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ws_id) = self.workspaces.workspace_of_tab(id) else {
            return;
        };

        let Some(tab) = self
            .workspaces
            .tabs_of(ws_id)
            .and_then(|tabs| tabs.list().find(id))
        else {
            return;
        };

        let surface = tab.surface();
        let is_settings = surface.is_settings();
        let is_agent = surface.is_agent();
        let count = self.close_process_count(surface, cx);

        let last_tab = self
            .workspaces
            .tabs_of(ws_id)
            .is_some_and(|tabs| tabs.list().len() == 1);

        // The settings entry holds one tab that terminates nothing, so its
        // close control is the same gesture as dismissing the entry.
        if is_settings {
            self.close_workspace_now(ws_id, window, cx);

            return;
        }

        if last_tab {
            if self.workspaces.real_len() == 1 {
                self.confirm_close_last_workspace(ws_id, window, cx);

                return;
            }

            let description = Self::close_description(
                count,
                if is_agent {
                    "shell-close-last-tab-agent-description"
                } else {
                    "shell-close-last-tab-description"
                },
                "shell-close-last-tab-processes-description",
            );

            Self::open_close_confirm(
                window,
                cx,
                t!("shell-close-last-tab-title"),
                description,
                None,
                move |this, window, cx| this.close_workspace_now(ws_id, window, cx),
            );

            return;
        }

        let settings = cx.global::<AppSettings>();
        let warn_before_terminating_shell = settings.config().system.warn_before_terminating_shell;

        if !should_confirm_close(
            is_agent && settings.config().system.confirm_before_closing_workspace,
            warn_before_terminating_shell,
            &count,
        ) {
            self.close_tab_now(id, window, cx);

            return;
        }

        let description = if is_agent {
            t!("shell-close-tab-agent-description").to_string()
        } else {
            Self::close_description(
                count,
                "shell-close-tab-description",
                "shell-close-tab-processes-description",
            )
        };

        Self::open_close_confirm(
            window,
            cx,
            t!("shell-close-tab-title"),
            description,
            None,
            move |this, window, cx| this.close_tab_now(id, window, cx),
        );
    }

    fn close_tab_now(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        let was_active = self.workspaces.active_tabs().list().active_id() == id;

        // `close` refuses the last tab of its workspace and returns the removed
        // pane entity; dropping it drops the pane's surface and PTY.
        let removed = self
            .workspaces
            .tabs_for_tab_mut(id)
            .and_then(|tabs| tabs.close(id));

        let Some(tree) = removed else {
            return;
        };

        for route in Self::agent_routes_in_surface(&tree, cx) {
            self.remove_agent_route(&route, cx);
        }

        drop(tree);

        if was_active {
            self.on_active_tab_changed(window, cx);
        }

        self.focus_active(window, cx);

        self.sync_session_memory(cx);

        cx.notify();
    }

    /// Close the workspace, with a confirm dialog first when the
    /// confirm-before-closing-workspace setting is on, or (with it off) when
    /// any of its shells has running child processes.
    pub(crate) fn request_close_workspace(
        &mut self,
        id: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.workspaces.is_pinned(id) {
            return;
        }

        // Dismissing the settings entry ends nothing the user could lose, so
        // it closes on the first click whatever the confirmation settings say.
        if self.workspaces.kind_of(id) == Some(WorkspaceKind::Settings) {
            self.close_workspace_now(id, window, cx);

            return;
        }

        if self.workspaces.real_len() == 1 {
            self.confirm_close_last_workspace(id, window, cx);

            return;
        }

        let confirm_before_closing_workspace = cx
            .global::<AppSettings>()
            .config()
            .system
            .confirm_before_closing_workspace;

        let count = self.workspace_process_count(id, cx);

        let warn_before_terminating_shell = cx
            .global::<AppSettings>()
            .config()
            .system
            .warn_before_terminating_shell;

        if !should_confirm_close(
            confirm_before_closing_workspace,
            warn_before_terminating_shell,
            &count,
        ) {
            self.close_workspace_now(id, window, cx);

            return;
        }

        let description = Self::close_description(
            count,
            "shell-close-workspace-description",
            "shell-close-workspace-processes-description",
        );

        Self::open_close_confirm(
            window,
            cx,
            t!("shell-close-workspace-title"),
            description,
            None,
            move |this, window, cx| this.close_workspace_now(id, window, cx),
        );
    }

    /// Close every temporary normal workspace through one confirmation. The
    /// aggregate process count keeps the existing subprocess warning setting
    /// effective without presenting one dialog per workspace.
    pub(crate) fn request_close_temporary_workspaces(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ids = self.workspaces.temporary_ids().collect::<Vec<_>>();

        if ids.is_empty() {
            return;
        }

        let process_count: io::Result<usize> = ids
            .iter()
            .map(|id| self.workspace_process_count(*id, cx))
            .sum();

        let settings = cx.global::<AppSettings>();

        let confirm_before_closing_workspace =
            settings.config().system.confirm_before_closing_workspace;

        let warn_before_terminating_shell = settings.config().system.warn_before_terminating_shell;

        if !should_confirm_close(
            confirm_before_closing_workspace,
            warn_before_terminating_shell,
            &process_count,
        ) {
            self.close_temporary_workspaces_now(&ids, window, cx);

            return;
        }

        let description = Self::close_description(
            process_count,
            "shell-close-temporary-workspaces-description",
            "shell-close-temporary-workspaces-processes-description",
        );

        Self::open_close_confirm(
            window,
            cx,
            t!("shell-close-temporary-workspaces-title"),
            description,
            None,
            move |this, window, cx| this.close_temporary_workspaces_now(&ids, window, cx),
        );
    }

    fn close_temporary_workspaces_now(
        &mut self,
        ids: &[WorkspaceId],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if ids.len() == self.workspaces.real_len() {
            let home = home_dir()
                .map(|home| home.display().to_string())
                .unwrap_or_else(|| ".".to_string());

            self.create_workspace(String::new(), WorkspaceRoots::single(home), window, cx);
        }

        for &id in ids {
            // This command explicitly includes every temporary workspace, so a
            // pin cannot leave an otherwise hidden temporary entry behind.
            self.workspaces.set_pinned(id, false);

            self.close_workspace_now(id, window, cx);
        }
    }

    /// Closing the last workspace is a three-way choice: quit the app (the
    /// workspace is then dropped from local_state, since the user asked to
    /// close it), swap in a fresh home-directory workspace, or cancel (a
    /// no-op: the workspace stays open and persists normally).
    fn confirm_close_last_workspace(
        &mut self,
        id: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let count = self.workspace_process_count(id, cx);

        let message = Self::close_description(
            count,
            "shell-close-last-workspace-message",
            "shell-close-last-workspace-processes-message",
        );

        // Quitting from here saves the session, so the same warning the
        // window-close dialog carries applies to this choice too.
        let note = self.temporary_workspace_note();

        let shell = cx.entity();

        window.open_dialog(cx, move |dialog, _, _| {
            close_last_workspace_dialog(dialog, &shell, id, &message, &note)
        });
    }

    /// Exclude `id` from session persistence and push the trimmed session to
    /// the registry so the quit hook saves local_state without it.
    fn doom_workspace(&mut self, id: WorkspaceId, cx: &mut Context<Self>) {
        self.doomed_workspace = Some(id);

        self.sync_session_memory(cx);
    }

    /// Swap the last workspace for a fresh default one rooted in the user's
    /// home directory, then close the old one (creation first, so the
    /// never-empty invariant of `WorkspaceManager` holds throughout).
    fn replace_last_workspace(
        &mut self,
        id: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let home = home_dir()
            .map(|home| home.display().to_string())
            .unwrap_or_else(|| ".".to_string());

        self.create_temporary_workspace(String::new(), WorkspaceRoots::single(home), window, cx);

        self.close_workspace_now(id, window, cx);
    }

    /// True when the window may close right away. The explicit confirmation
    /// setting and terminal child-process warnings share this path. Reached
    /// from the titlebar X and the OS close request (Alt+F4, taskbar).
    pub(crate) fn confirm_window_close(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let saved = ui::settings::save_settings(window, cx);

        let count: io::Result<usize> = self
            .workspaces
            .all_tabs()
            .flat_map(|tabs| tabs.list().items())
            .map(|tab| self.close_process_count(tab.surface(), cx))
            .sum();

        let settings = cx.global::<AppSettings>();
        let warn_before_terminating_shell = settings.config().system.warn_before_terminating_shell;

        if saved
            && !should_confirm_close(
                settings.config().system.confirm_before_closing_workspace,
                warn_before_terminating_shell,
                &count,
            )
        {
            return true;
        }

        let mut description = Self::close_description(
            count,
            "shell-close-window-description",
            "shell-close-window-processes-description",
        );

        if !saved {
            description.push_str("\n\n");

            description.push_str(&t!("settings-save-failed-close-description"));
        }

        let note = self.temporary_workspace_note();

        // `remove_window` tears the window down directly (no WM_CLOSE
        // round-trip), so this dialog won't re-trigger.
        if !saved {
            window.open_alert_dialog(cx, move |alert, _, _| {
                alert
                    .title(t!("settings-save-failed-title"))
                    .description(
                        v_flex()
                            .gap_1()
                            .child(description.clone())
                            .children(note.clone().map(|note| div().font_bold().child(note))),
                    )
                    .button_props(
                        DialogButtonProps::default()
                            .show_cancel(true)
                            .ok_text(t!("settings-close-without-saving"))
                            .cancel_text(t!("shell-close-cancel")),
                    )
                    .on_ok(|_, window, cx| {
                        if cx.windows().len() == 1 {
                            cx.global_mut::<AppSettings>().discard_on_exit();
                        }

                        window.remove_window();

                        true
                    })
            });

            return false;
        }

        Self::open_close_confirm(
            window,
            cx,
            t!("shell-close-window-title"),
            description,
            note,
            |_, window, _| window.remove_window(),
        );

        false
    }

    fn close_workspace_now(
        &mut self,
        id: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let routes = self
            .workspaces
            .tabs_of(id)
            .map(|tabs| {
                tabs.list()
                    .items()
                    .iter()
                    .flat_map(|tab| Self::agent_routes_in_surface(tab.surface(), cx))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let settings = self.workspaces.kind_of(id) == Some(WorkspaceKind::Settings);

        if settings && !ui::settings::save_settings(window, cx) {
            return;
        }

        let was_active = self.workspaces.list().active_id() == id;

        if self.workspaces.close_workspace(id).is_some() {
            for route in routes {
                self.remove_agent_route(&route, cx);
            }

            if settings {
                self.retire_settings_workspace(cx);
            }

            if was_active {
                self.on_active_tab_changed(window, cx);
            }

            self.focus_active(window, cx);

            self.sync_session_memory(cx);

            cx.notify();
        }
    }

    pub(super) fn on_split_up(&mut self, _: &SplitUp, window: &mut Window, cx: &mut Context<Self>) {
        self.split_pane(SplitDirection::Up, window, cx);
    }

    pub(super) fn on_split_down(
        &mut self,
        _: &SplitDown,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.split_pane(SplitDirection::Down, window, cx);
    }

    pub(super) fn on_split_left(
        &mut self,
        _: &SplitLeft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.split_pane(SplitDirection::Left, window, cx);
    }

    pub(super) fn on_split_right(
        &mut self,
        _: &SplitRight,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.split_pane(SplitDirection::Right, window, cx);
    }

    /// Create a new pane on the given side of the focused pane. The new shell
    /// starts in the focused pane's live cwd (OSC 7 when reported, launch cwd
    /// otherwise) and becomes the focused pane. A no-op when the focused pane
    /// cannot yield the minimum panel size to the new sibling.
    fn split_pane(
        &mut self,
        direction: SplitDirection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Agent tabs have no pane tree to split.
        let Some(focused) = self.try_active_pane() else {
            return;
        };

        // Avoid spawning a shell when its panel cannot fit beside this pane.
        let has_room = focused.read(cx).content_size().is_none_or(|size| {
            let extent = match direction.into() {
                Axis::Horizontal => size.width,
                Axis::Vertical => size.height,
            };

            px(extent.as_f32() / 2.0) >= PANEL_MIN_SIZE
        });

        if !has_room {
            return;
        }

        let cwd = focused.read(cx).tab_state().cwd;
        let id = Self::alloc_id(&mut self.next_id);
        let default_profile = Self::default_profile(cx);

        let pane = spawn_default_pane(cx, id, default_profile, cwd);

        let tree = self.workspaces.active_tabs_mut().active_mut().live_mut();

        if !tree.split(PaneId(id), pane.clone(), direction, cx) {
            return;
        }

        self.register_agent_pane(&pane, cx);

        self.on_active_tab_changed(window, cx);

        self.focus_active(window, cx);

        self.sync_session_memory(cx);

        cx.notify();
    }

    pub(super) fn on_resize_pane_up(
        &mut self,
        _: &ResizePaneUp,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.resize_pane(SplitDirection::Up, window, cx);
    }

    pub(super) fn on_resize_pane_down(
        &mut self,
        _: &ResizePaneDown,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.resize_pane(SplitDirection::Down, window, cx);
    }

    pub(super) fn on_resize_pane_left(
        &mut self,
        _: &ResizePaneLeft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.resize_pane(SplitDirection::Left, window, cx);
    }

    pub(super) fn on_resize_pane_right(
        &mut self,
        _: &ResizePaneRight,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.resize_pane(SplitDirection::Right, window, cx);
    }

    /// Resize the focused pane one step along the arrow's axis, in the nearest
    /// ancestor split with a matching axis (tmux semantics: the trailing edge
    /// moves, except for the last child whose only movable edge is the leading
    /// one). A no-op when no matching-axis split exists.
    fn resize_pane(
        &mut self,
        direction: SplitDirection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tree) = self.workspaces.active_tabs().active().tree() else {
            return;
        };

        if !tree.resize(direction, PANE_RESIZE_STEP, window, cx) {
            return;
        }

        self.sync_session_memory(cx);

        cx.notify();
    }

    /// Focus the pane `id` in the active tab (mouse click).
    pub(crate) fn focus_pane(&mut self, id: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tree) = self.workspaces.active_tabs_mut().active_mut().tree_mut() else {
            return;
        };

        if tree.tree().focused() == id || !tree.tree_mut().set_focused(id) {
            return;
        }

        self.on_active_tab_changed(window, cx);

        self.focus_active(window, cx);

        self.sync_session_memory(cx);

        cx.notify();
    }

    /// Apply saved split ratios once their groups have real bounds (the first
    /// visible frames after a session restore); cleared after applying.
    pub(super) fn apply_pending_ratios(&mut self, cx: &mut Context<Self>) {
        let Some(tree) = self.workspaces.active_tabs_mut().active_mut().tree_mut() else {
            return;
        };

        tree.apply_pending_ratios(cx);
    }

    /// The active tab's pane tree as nested resizable groups. The main surface
    /// owns the outer frame, so a single pane renders without another card.
    pub(super) fn render_active_tree(&self, cx: &mut Context<Self>) -> AnyElement {
        match self.workspaces.active_tabs().active() {
            TabSurface::Team(pane) => {
                return div()
                    .size_full()
                    .overflow_hidden()
                    .child(pane.clone())
                    .into_any_element();
            }
            TabSurface::TeamUnavailable { message, .. } => {
                return div()
                    .size_full()
                    .p_4()
                    .child(message.clone())
                    .into_any_element();
            }
            TabSurface::TeamDisabled(_) => {
                return div()
                    .size_full()
                    .p_4()
                    .child(t!("team-disabled").into_owned())
                    .into_any_element();
            }
            _ => {}
        }

        if self.workspaces.active_tabs().active().is_settings() {
            let settings = self.settings.render(cx);

            return div()
                .size_full()
                .overflow_hidden()
                // The Settings widget paints no fill of its own, so without
                // this the translucent surface card shows the window backdrop
                // through the page area while the sidebar, which carries an
                // explicit fill, stays opaque.
                .bg(cx
                    .theme()
                    .background
                    .alpha(main_view_background_opacity(cx)))
                .children(settings)
                .into_any_element();
        }

        if let Some(agent) = self.active_agent() {
            return div()
                .size_full()
                .overflow_hidden()
                .child(agent)
                .into_any_element();
        }

        let tree = self.workspaces.active_tabs().active().live();

        let multi = !tree.tree().is_single_leaf();

        Self::render_pane_node(tree.tree().root(), tree.tree().focused(), multi, cx)
    }

    fn render_pane_node(
        node: &PaneNode<Entity<TerminalPane>>,
        focused: PaneId,
        multi: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match node {
            PaneNode::Leaf { id, pane, .. } => {
                let id = *id;

                div()
                    .size_full()
                    // Split leaves retain equal-width borders so focus changes
                    // never shift layout; the parent surface clips their outer
                    // edges and provides the single-pane frame.
                    .when(multi, |this| {
                        this.border_1().border_color(if id == focused {
                            cx.theme().primary
                        } else {
                            cx.theme().border
                        })
                    })
                    .capture_any_mouse_down(cx.listener(
                        move |this, _: &MouseDownEvent, window, cx| {
                            this.focus_pane(id, window, cx);
                        },
                    ))
                    .child(pane.clone())
                    .into_any_element()
            }
            PaneNode::Split {
                id,
                axis,
                children,
                state,
                ..
            } => {
                let shell = cx.entity();

                let mut group = ResizablePanelGroup::new(("pane-split", *id as usize))
                    .axis(*axis)
                    .with_state(state)
                    // Keep the in-memory session mirror's split ratios fresh
                    // after divider drags (the quit hook reads it).
                    .on_resize(move |_, _, cx| {
                        shell.update(cx, |this, cx| this.sync_session_memory(cx));
                    });

                for child in children {
                    group = group.child(
                        resizable_panel().child(Self::render_pane_node(child, focused, multi, cx)),
                    );
                }

                group.into_any_element()
            }
        }
    }

    fn insert_tab(
        &mut self,
        id: TabId,
        surface: TabSurface,
        title: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.workspaces
            .active_tabs_mut()
            .new_tab(surface, id, title);

        self.on_active_tab_changed(window, cx);

        self.focus_active(window, cx);

        self.sync_session_memory(cx);

        cx.notify();
    }

    pub(crate) fn open_team_tab(
        &mut self,
        saved: Option<RoomId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !cx.global::<AppSettings>().config().agent.enable_agent_team {
            return;
        }

        self.leave_settings_workspace();

        let directory = config_dir_path();

        let runtime = match saved {
            Some(id) => TeamRuntime::open(&directory, id, cx),
            None => TeamRuntime::create(
                &directory,
                agent_workspace(self.workspaces.active_roots()),
                cx,
            ),
        };

        let surface = match runtime {
            Ok(runtime) => TabSurface::Team(cx.new(|cx| TeamPane::new(runtime, window, cx))),
            Err(error) => TabSurface::TeamUnavailable {
                saved: Box::new(TabState {
                    team_room: saved.map(|id| id.to_string()),
                    ..TabState::default()
                }),
                message: error.to_string(),
            },
        };

        let id = Self::alloc_id(&mut self.next_id);

        self.insert_tab(
            TabId(id),
            surface,
            t!("team-title").into_owned(),
            window,
            cx,
        );
    }

    pub(crate) fn on_new_team_tab(
        &mut self,
        _: &NewTeamTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_team_tab(None, window, cx);
    }

    pub(super) fn default_profile(cx: &Context<Self>) -> (Option<String>, Vec<String>) {
        cx.global::<AppSettings>().default_profile_command()
    }

    /// Open a new window with a fresh default session, offset from this one so
    /// the two don't exactly overlap.
    pub(super) fn on_new_window(
        &mut self,
        _: &NewWindow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let bounds = window.window_bounds().get_bounds();

        AppWindow::open(
            cx,
            AppWindow {
                bounds: Some(WindowState {
                    x: bounds.origin.x.as_f32() + 30.0,
                    y: bounds.origin.y.as_f32() + 30.0,
                    width: bounds.size.width.as_f32(),
                    height: bounds.size.height.as_f32(),
                    maximized: false,
                }),
                session: None,
                // New windows inherit this window's sidebar width.
                sidebar_width: Some(self.sidebar.width),
                initial_cwd: None,
            },
        );
    }

    pub(crate) fn on_new_tab(&mut self, _: &NewTab, window: &mut Window, cx: &mut Context<Self>) {
        let default_profile = Self::default_profile(cx);

        self.open_profile_tab(default_profile, window, cx);
    }

    /// Open a terminal tab running the given launch command (a profile picked
    /// from the new-tab menu, or the default profile) in the active
    /// workspace's primary directory.
    pub(crate) fn open_profile_tab(
        &mut self,
        profile: (Option<String>, Vec<String>),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // `leave_settings_workspace` can change which workspace is active, so
        // the primary directory is read only after the move.
        self.leave_settings_workspace();

        let cwd = self.workspaces.active_cwd().to_string();

        self.open_profile_tab_in_directory(profile, cwd, window, cx);
    }

    /// Open a terminal tab running `profile` with `cwd` as the process working
    /// directory. Starting a process somewhere is not the same as the
    /// workspace living there, so this leaves the workspace's own directories
    /// untouched.
    pub(crate) fn open_profile_tab_in_directory(
        &mut self,
        profile: (Option<String>, Vec<String>),
        cwd: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.leave_settings_workspace();

        let id = Self::alloc_id(&mut self.next_id);

        let pane = spawn_default_pane(cx, id, profile, explicit_cwd(&cwd));

        self.register_agent_pane(&pane, cx);

        let title = pane.read(cx).profile_name().to_string();

        self.insert_tab(
            TabId(id),
            TabSurface::Live(TerminalLayout::new_leaf(PaneId(id), pane)),
            title,
            window,
            cx,
        );
    }

    /// Open a remote-session tab: connect to a paired host in the background,
    /// then add a tab whose terminal is fed over the network by `NetPty`.
    #[cfg(windows)]
    pub(crate) fn on_new_remote_tab(
        &mut self,
        _: &NewRemoteTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let hosts = remote::known_hosts();

        let Some(host) = hosts.into_iter().next() else {
            window.push_notification(t!("shell-remote-no-hosts"), cx);

            return;
        };

        // Connects to the first paired host; a host picker is only meaningful
        // once a user keeps several hosts paired at the same time.
        let id = Self::alloc_id(&mut self.next_id);

        cx.spawn_in(window, async move |this, cx| {
            let connected = cx
                .background_executor()
                .spawn(async move { remote::connect_new_session(&host) })
                .await;

            let _ = this.update_in(cx, |this, window, cx| match connected {
                Ok(remote) => match attach_remote(cx, id, remote) {
                    Ok(pane) => {
                        this.leave_settings_workspace();

                        this.register_agent_pane(&pane, cx);

                        this.insert_tab(
                            TabId(id),
                            TabSurface::Live(TerminalLayout::new_leaf(PaneId(id), pane)),
                            t!("shell-remote-tab-title").to_string(),
                            window,
                            cx,
                        );
                    }
                    Err(e) => {
                        window.push_notification(
                            t!("shell-remote-session-failed", error = e)
                                .into_owned()
                                .as_str(),
                            cx,
                        );
                    }
                },
                Err(e) => {
                    window.push_notification(
                        t!("shell-remote-connect-failed", error = e)
                            .into_owned()
                            .as_str(),
                        cx,
                    );
                }
            });
        })
        .detach();
    }

    /// Open an agent tab: an agent chat conversation in place of a terminal.
    /// The conversation's agent process starts in the workspace cwd.
    pub(crate) fn open_agent_tab(
        &mut self,
        profile: AgentProfile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.leave_settings_workspace();

        let workspace = agent_workspace(self.workspaces.active_roots());

        self.open_agent_tab_in(&profile, workspace, None, window, cx);
    }

    /// Open an agent tab rooted at `cwd`, optionally continuing `resume` once
    /// its session starts. A conversation belongs to the directory it ran in,
    /// so one listed from another tab opens here rather than in the tab that
    /// listed it.
    pub(super) fn open_agent_tab_in(
        &mut self,
        profile: &AgentProfile,
        workspace: AgentWorkspace,
        resume: Option<RecoveryIdentity>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = Self::alloc_id(&mut self.next_id);

        // The tab is titled by the profile so multiple profiles of the same
        // agent stay distinguishable; an unnamed profile falls back to the
        // agent name.
        let title = if profile.name.trim().is_empty() {
            profile.kind.display().to_string()
        } else {
            profile.name.clone()
        };

        let owner = AgentSession::create(profile.clone(), workspace, None, cx);
        let pane = cx.new(|cx| AgentPane::attach(&owner, window, cx));

        Self::watch_agent_tab(&pane, cx);

        self.register_agent_tab(&pane, cx);

        owner.start(resume, cx);

        self.insert_tab(
            TabId(id),
            TabSurface::Agent(AgentTab { owner, pane }),
            title,
            window,
            cx,
        );
    }

    pub(crate) fn on_new_agent_tab(
        &mut self,
        _: &NewAgentTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let profile = cx.global::<AppSettings>().default_agent_profile_entry();

        self.open_agent_tab(profile, window, cx);
    }

    /// CLI `new_tab`: reuse the workspace rooted exactly at `path`, otherwise
    /// open a fresh workspace there. With `open_in_best_workspace` on, a
    /// containing workspace is preferred over a new one and gets the tab
    /// instead, with the shell started in `path`.
    pub(crate) fn open_dir_tab(
        &mut self,
        path: &path::Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let summaries = self.workspaces.summaries();

        if let Some(index) = exact_match(&summaries, path).and_then(|workspace_id| {
            summaries
                .iter()
                .position(|workspace| workspace.id == workspace_id)
        }) {
            // Workspace activation preserves its TabManager's active index,
            // restoring the tab the user last used without spawning a shell.
            self.workspaces.list_mut().activate(index);

            window.activate_window();

            self.on_active_tab_changed(window, cx);

            self.focus_active(window, cx);

            self.sync_session_memory(cx);

            cx.notify();

            return;
        }

        let target = path.display().to_string();

        let containing = cx
            .global::<AppSettings>()
            .config()
            .system
            .open_in_best_workspace
            .then(|| best_match(&self.workspaces.summaries(), path))
            .flatten();

        let Some(ws_id) = containing else {
            self.create_temporary_workspace(
                t!("shell-workspace-default-name").into(),
                WorkspaceRoots::single(target),
                window,
                cx,
            );

            return;
        };

        if let Some(index) = self
            .workspaces
            .summaries()
            .iter()
            .position(|ws| ws.id == ws_id)
        {
            self.workspaces.list_mut().activate(index);
        }

        let default_profile = Self::default_profile(cx);

        self.open_profile_tab_in_directory(default_profile, target, window, cx);
    }

    pub(crate) fn set_workspace_pinned(
        &mut self,
        id: WorkspaceId,
        pinned: bool,
        cx: &mut Context<Self>,
    ) {
        self.workspaces.set_pinned(id, pinned);

        self.sync_session_memory(cx);

        cx.notify();
    }

    pub(crate) fn reorder_workspaces(
        &mut self,
        from: usize,
        to: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.workspaces.reorder(from, to);

        self.focus_active(window, cx);

        self.sync_session_memory(cx);

        cx.notify();
    }

    /// Move a tab within the workspace that owns it. The tab id picks the tab
    /// manager, so this reaches a workspace the user is not currently in.
    pub(crate) fn reorder_tab(
        &mut self,
        tab: TabId,
        from: usize,
        to: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tabs) = self.workspaces.tabs_for_tab_mut(tab) else {
            return;
        };

        tabs.list_mut().reorder(from, to);

        self.focus_active(window, cx);

        self.sync_session_memory(cx);

        cx.notify();
    }

    pub(super) fn on_next_tab(&mut self, _: &NextTab, window: &mut Window, cx: &mut Context<Self>) {
        self.workspaces.active_tabs_mut().list_mut().focus_next();

        self.on_active_tab_changed(window, cx);

        self.focus_active(window, cx);

        self.sync_session_memory(cx);

        cx.notify();
    }

    pub(super) fn on_prev_tab(&mut self, _: &PrevTab, window: &mut Window, cx: &mut Context<Self>) {
        self.workspaces.active_tabs_mut().list_mut().focus_prev();

        self.on_active_tab_changed(window, cx);

        self.focus_active(window, cx);

        self.sync_session_memory(cx);

        cx.notify();
    }

    /// Position of the next tab `marked` accepts, searching after the active
    /// tab and wrapping in workspace-then-tab order so repeated jumps walk the
    /// whole marked set in turn.
    fn next_marked_tab(&self, marked: impl Fn(&Tab<TabSurface>) -> bool) -> Option<(usize, usize)> {
        let (positions, marks): (Vec<(usize, usize)>, Vec<bool>) = self
            .workspaces
            .all_tabs()
            .enumerate()
            .flat_map(|(workspace_index, tabs)| {
                tabs.list()
                    .items()
                    .iter()
                    .enumerate()
                    .map(move |(tab_index, tab)| ((workspace_index, tab_index), tab))
            })
            .map(|(position, tab)| (position, marked(tab)))
            .unzip();

        let active = (
            self.workspaces.list().active_index(),
            self.workspaces.active_tabs().list().active_index(),
        );

        let active_position = positions
            .iter()
            .position(|&position| position == active)
            .unwrap_or(0);

        next_marked_position(&marks, active_position).map(|index| positions[index])
    }

    /// The next tab holding something the user has not looked at: a background
    /// command that finished, or an unread agent reply.
    ///
    /// Each jump shrinks the set rather than advancing a cursor of its own,
    /// because focusing a tab is what clears both marks.
    pub(super) fn next_ready_tab(&self, cx: &App) -> Option<(usize, usize)> {
        self.next_marked_tab(|tab| {
            let routes = Self::agent_routes_in_surface(tab.surface(), cx);

            tab.last_outcome().is_some()
                || self
                    .agent_notifications
                    .agent_monitor
                    .project(&routes)
                    .unread_count
                    > 0
        })
    }

    /// The next tab with work still in flight: a terminal running a command, or
    /// an agent still producing its answer.
    ///
    /// Unlike the ready set this one does not shrink when the tab is focused —
    /// watching a tab does not finish its work — so the jump keeps cycling
    /// while the same tabs stay busy, which is what following several parallel
    /// runs needs.
    pub(super) fn next_busy_tab(&self, cx: &App) -> Option<(usize, usize)> {
        self.next_marked_tab(|tab| {
            if Self::tab_terminal_activity(tab, cx) == TerminalActivity::Running {
                return true;
            }

            let routes = Self::agent_routes_in_surface(tab.surface(), cx);

            self.agent_notifications
                .agent_monitor
                .project(&routes)
                .status
                == AgentRuntimeStatus::Running
        })
    }

    pub(super) fn jump_to_tab(
        &mut self,
        workspace_index: usize,
        tab_index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.workspaces.list_mut().activate(workspace_index);

        self.workspaces
            .active_tabs_mut()
            .list_mut()
            .activate(tab_index);

        self.on_active_tab_changed(window, cx);

        self.focus_active(window, cx);

        self.sync_session_memory(cx);

        cx.notify();
    }

    /// Open the new-workspace dialog: a name plus the shared directory editor,
    /// so a workspace can be created with several directories in one step.
    /// Confirming creates the workspace; cancel creates nothing.
    pub(crate) fn on_new_workspace(
        &mut self,
        _: &NewWorkspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(t!("shell-workspace-default-name").to_string())
        });

        // A new workspace starts with no directory at all, which the editor's
        // non-empty invariant cannot express; the picker fills the first one
        // in and Create stays refused until it does.
        let dirs = cx.new(|cx| WorkspaceDirsEditor::new(None, cx));

        let shell = cx.entity();

        window.open_dialog(cx, move |dialog, window, _| {
            new_workspace_dialog(dialog, &name_input, &dirs, &shell, window)
        });
    }

    /// Adopt a temporary workspace: from here on it is saved with the session
    /// like any other. Already-persistent workspaces are unaffected.
    pub(crate) fn activate_as_workspace(&mut self, id: WorkspaceId, cx: &mut Context<Self>) {
        self.workspaces.set_temporary(id, false);

        self.sync_session_memory(cx);

        cx.notify();
    }

    /// Create a workspace for a directory the user only asked to open, such as
    /// a drop or an external open request. It stays out of the saved session
    /// until the sidebar's "activate" action adopts it, so running one command
    /// somewhere does not grow the session behind the user's back.
    pub(super) fn create_temporary_workspace(
        &mut self,
        name: String,
        roots: WorkspaceRoots,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = self.create_workspace(name, roots, window, cx);

        self.workspaces.set_temporary(id, true);

        self.sync_session_memory(cx);
    }

    /// Create a workspace named `name` (empty falls back to the shared default)
    /// whose shells start in `dir` (empty falls back to the default
    /// startup directory), seeded with one fresh tab, and activate it.
    ///
    /// The workspace is part of the saved session from the moment it exists,
    /// because asking for a workspace by name is already the decision to keep
    /// it.
    pub(super) fn create_workspace(
        &mut self,
        name: String,
        roots: WorkspaceRoots,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> WorkspaceId {
        let name = if name.is_empty() {
            t!("shell-workspace-default-name").to_string()
        } else {
            name
        };

        let cwd = explicit_cwd(roots.primary());
        let surface_id = Self::alloc_id(&mut self.next_id);
        let default_profile = Self::default_profile(cx);
        let pane = spawn_default_pane(cx, surface_id, default_profile, cwd.clone());

        self.register_agent_pane(&pane, cx);

        let title = pane.read(cx).profile_name().to_string();

        let tabs = TabManager::new(
            TabSurface::Live(TerminalLayout::new_leaf(PaneId(surface_id), pane)),
            TabId(surface_id),
            title,
        );

        let ws_id = Self::alloc_id(&mut self.next_id);

        let ws_id = self.workspaces.new_workspace_of_kind(
            tabs,
            WorkspaceId(ws_id),
            name,
            Some(roots),
            WorkspaceKind::Normal,
        );

        self.on_active_tab_changed(window, cx);

        self.focus_active(window, cx);

        self.refresh_root_availability(cx);

        self.sync_session_memory(cx);

        cx.notify();

        ws_id
    }

    pub(super) fn on_next_workspace(
        &mut self,
        _: &NextWorkspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let len = self.workspaces.list().len();
        let next = (self.workspaces.list().active_index() + 1) % len;

        self.workspaces.list_mut().activate(next);

        self.on_active_tab_changed(window, cx);

        self.focus_active(window, cx);

        self.sync_session_memory(cx);

        cx.notify();
    }

    pub(super) fn on_prev_workspace(
        &mut self,
        _: &PrevWorkspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let len = self.workspaces.list().len();
        let prev = (self.workspaces.list().active_index() + len - 1) % len;

        self.workspaces.list_mut().activate(prev);

        self.on_active_tab_changed(window, cx);

        self.focus_active(window, cx);

        self.sync_session_memory(cx);

        cx.notify();
    }

    /// Start renaming a workspace inline in the sidebar: the item swaps its
    /// name for an input pre-filled with the current name.
    pub(crate) fn start_workspace_rename(
        &mut self,
        id: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(current) = self
            .workspaces
            .summaries()
            .into_iter()
            .find(|ws| ws.id == id)
            .map(|ws| ws.name)
        else {
            return;
        };

        self.renames.begin_workspace(id, current, window, cx);

        cx.notify();
    }

    /// End the in-flight workspace rename. Enter and blur commit the entered
    /// name (blank names are dropped by the manager); Escape reaches this
    /// with `commit` false, keeping the original name.
    pub(crate) fn finish_workspace_rename(
        &mut self,
        commit: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((id, input)) = self.renames.take_workspace() else {
            return;
        };

        if commit {
            let name = input.read(cx).value().trim().to_string();

            self.workspaces.rename(id, name);

            self.sync_session_memory(cx);
        }

        self.focus_active(window, cx);

        cx.notify();
    }

    /// Start renaming a tab inline in the tab bar: the tab swaps its label
    /// for an input pre-filled with the current title.
    pub(crate) fn start_tab_rename(
        &mut self,
        id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(current) = self
            .workspaces
            .active_tabs()
            .list()
            .find(id)
            .map(|tab| tab.title().to_string())
        else {
            return;
        };

        self.renames.begin_tab(id, current, window, cx);

        cx.notify();
    }

    /// End the in-flight tab rename; same semantics as the workspace rename
    /// (Enter/blur commit, Escape cancels, blank names are dropped).
    pub(crate) fn finish_tab_rename(
        &mut self,
        commit: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((id, input)) = self.renames.take_tab() else {
            return;
        };

        if commit {
            let name = input.read(cx).value().trim().to_string();

            if !name.is_empty() {
                let mut renamed_agent = None;

                if let Some(tabs) = self.workspaces.tabs_for_tab_mut(id) {
                    tabs.rename(id, name.clone());

                    renamed_agent = tabs
                        .list()
                        .find(id)
                        .and_then(|tab| tab.surface().agent().cloned());

                    self.sync_session_memory(cx);
                }

                // An agent tab's name is the conversation's name, so it goes
                // to the harness too: its session listing is what this
                // application's own recent-sessions list reads.
                if let Some(agent) = renamed_agent {
                    agent.update(cx, |agent, _| agent.rename_session(&name));
                }
            }
        }

        self.focus_active(window, cx);

        cx.notify();
    }

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
            self.panels.sync_agent_targets(self.active_agent(), cx);

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
            self.panels.sync_agent_targets(self.active_agent(), cx);
        }

        // Git owns the poller's own visibility flag; leaving Git for another
        // view stops the polling it turned on.
        self.panels.set_git_sidebar_open(false, cx);

        cx.notify();
    }

    /// Show settings as a pseudo workspace: a sidebar entry holding a single
    /// `Settings` tab whose surface fills the main area. A modal would block
    /// the terminal the user is adjusting settings for, while an entry can be
    /// left open and switched away from.
    ///
    /// Field edits mutate the `AppSettings` global live (for preview); the set
    /// is written when the entry closes and again on quit, since an entry the
    /// user never closes would otherwise never reach the file.
    pub(super) fn on_show_settings(
        &mut self,
        _: &ShowSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(id) = self.workspaces.settings_id() {
            if let Some(index) = self
                .workspaces
                .summaries()
                .iter()
                .position(|ws| ws.id == id)
            {
                self.workspaces.list_mut().activate(index);

                self.on_active_tab_changed(window, cx);

                self.focus_active(window, cx);

                cx.notify();
            }

            return;
        }

        self.settings.open(window, cx);

        let id = Self::alloc_id(&mut self.next_id);

        let tabs = TabManager::new(
            TabSurface::Settings,
            TabId(id),
            settings_title().to_string(),
        );

        let ws_id = Self::alloc_id(&mut self.next_id);

        self.workspaces.new_workspace_of_kind(
            tabs,
            WorkspaceId(ws_id),
            settings_title().to_string(),
            // The settings entry is a view of the configuration file, so it
            // owns no directory and never contributes to path routing.
            None,
            WorkspaceKind::Settings,
        );

        self.on_active_tab_changed(window, cx);

        self.focus_active(window, cx);

        cx.notify();
    }

    /// Drop the settings surface after its edits have been saved successfully.
    /// Reached from every path that removes the settings entry.
    pub(super) fn retire_settings_workspace(&mut self, _cx: &mut Context<Self>) {
        // Pick up relay URL / token edits made while the entry was open.
        #[cfg(windows)]
        ui::settings::reconcile_remote_host(_cx);

        self.settings.retire();
    }

    /// Leave the settings entry for a normal workspace. Every path that adds a
    /// tab funnels through this, so a new tab never lands in the settings
    /// entry and breaks its single-tab presentation.
    pub(crate) fn leave_settings_workspace(&mut self) {
        if self.workspaces.active_kind() == WorkspaceKind::Settings {
            let index = self.workspaces.first_normal_index();

            self.workspaces.list_mut().activate(index);
        }
    }

    /// Keep background host events and accepted grid changes in the saved session.
    pub(crate) fn watch_pane(pane: &Entity<TerminalPane>, cx: &mut Context<Self>) {
        cx.observe(pane, |this, pane, cx| this.on_pane_notified(pane, cx))
            .detach();

        cx.subscribe(pane, Self::on_agent_interrupted).detach();

        cx.subscribe(pane, |this, _, _: &TerminalGridResized, cx| {
            this.sync_session_memory(cx);
        })
        .detach();
    }

    fn on_agent_interrupted(
        &mut self,
        pane: Entity<TerminalPane>,
        _: &AgentInterrupted,
        cx: &mut Context<Self>,
    ) {
        let route = pane.read(cx).agent_route().clone();

        let mutation = self
            .agent_notifications
            .agent_monitor
            .interrupt(&route, time::Instant::now());

        Self::apply_agent_monitor_display_change(&mutation, cx);

        self.agent_notifications.reschedule_agent_timer(cx);
    }

    /// The id of the tab whose pane tree contains `pane_id`, searched across
    /// all workspaces (pane ids and tab ids come from the same counter but are
    /// no longer equal once a tab is split).
    fn tab_for_pane(&self, pane_id: PaneId) -> Option<TabId> {
        self.workspaces.find_tab_id(|tree| tree.contains(pane_id))
    }

    /// Host-event pump: drain one pane's events (applying its pane-side effects)
    /// and fold the chrome-visible ones into the owning tab.
    fn on_pane_notified(&mut self, pane: Entity<TerminalPane>, cx: &mut Context<Self>) {
        let pane_id = PaneId(pane.read(cx).id());
        let agent_route = pane.read(cx).agent_route().clone();
        let events = pane.update(cx, |pane, _cx| pane.drain_host_events());

        let mut chrome_changed = false;
        let mut session_changed = false;

        for event in &events {
            match event {
                HostEvent::Title(title) => {
                    if let Some(tab_id) = self.tab_for_pane(pane_id)
                        && self
                            .workspaces
                            .tabs_for_tab(tab_id)
                            .and_then(|tabs| tabs.list().find(tab_id))
                            .is_some_and(|tab| {
                                tab.surface()
                                    .tree()
                                    .is_some_and(|t| t.tree().focused() == pane_id)
                            })
                        && let Some(tabs) = self.workspaces.tabs_for_tab_mut(tab_id)
                    {
                        chrome_changed |= tabs.set_title(tab_id, title.clone());
                    }
                }
                HostEvent::Exit => {
                    self.remove_agent_route(&agent_route, cx);

                    if let Some(tab_id) = self.tab_for_pane(pane_id) {
                        // A pane whose shell exits auto-closes when the tab has
                        // other panes (the split collapses around it); the last
                        // pane lingers read-only and marks the tab exited, as
                        // before splits existed.
                        let mut removed = None;

                        if let Some(tabs) = self.workspaces.tabs_for_tab_mut(tab_id) {
                            if let Some(tab) = tabs.list_mut().find_mut(tab_id)
                                && tab
                                    .surface()
                                    .tree()
                                    .is_some_and(|t| !t.tree().is_single_leaf())
                            {
                                removed = tab.surface_mut().live_mut().remove(pane_id, cx);
                            }

                            if removed.is_none() {
                                tabs.mark_exited(tab_id);
                            }

                            // A dead command sends no state-0 report, so its
                            // bar would otherwise sit at whatever it reached.
                            tabs.clear_progress(tab_id);
                        }

                        if let Some(pane) = removed {
                            // Dropping the pane entity releases its surface and
                            // ConPTY, same as an explicit pane close.
                            drop(pane);

                            // Re-run focus_active on the next render (the pump
                            // has no Window).
                            self.chrome.needs_focus = true;

                            self.sync_session_memory(cx);
                        }

                        chrome_changed = true;
                    }
                }
                HostEvent::Bell => {
                    // Only background tabs get the indicator: a bell on the tab
                    // in front of you is already conveyed by the sound and the
                    // output itself, and flagging it would need a timer to
                    // expire the flag again.
                    if let Some(tab_id) = self.tab_for_pane(pane_id)
                        && self.workspaces.active_tabs().list().active_id() != tab_id
                        && let Some(tabs) = self.workspaces.tabs_for_tab_mut(tab_id)
                    {
                        tabs.ring_bell(tab_id);

                        chrome_changed = true;
                    }
                }
                HostEvent::Progress(report) => {
                    if let Some(tab_id) = self.tab_for_pane(pane_id)
                        && let Some(tabs) = self.workspaces.tabs_for_tab_mut(tab_id)
                    {
                        tabs.set_progress(tab_id, *report);

                        chrome_changed = true;
                    }
                }
                HostEvent::CommandFinished { exit_code } => {
                    if let Some(tab_id) = self.tab_for_pane(pane_id) {
                        let watched = self.workspaces.active_tabs().list().active_id() == tab_id;

                        if let Some(tabs) = self.workspaces.tabs_for_tab_mut(tab_id) {
                            // Only a tab the user is not watching records its
                            // result: a command that ends in front of them
                            // already shows its own output, and the record
                            // would clear on activation anyway.
                            if !watched {
                                tabs.record_outcome(tab_id, (*exit_code).into());
                            }

                            // The bar belongs to the command that reported it.
                            // A command killed before it could finish (Ctrl-C
                            // during a build) sends no state-0 report, so its
                            // bar would otherwise sit at the fraction it
                            // reached until something else reported progress.
                            tabs.clear_progress(tab_id);
                        }
                    }

                    chrome_changed = true;
                }
                // A command starting flips the workspace indicator, which lives
                // in the chrome rather than in the pane's own grid.
                HostEvent::InteractiveState(_)
                | HostEvent::PromptBoundaryTrusted(_)
                | HostEvent::PromptStarted
                | HostEvent::CommandStarted => chrome_changed = true,
                HostEvent::Cwd(_) => session_changed = true,
                HostEvent::Notification { title, body } => {
                    let mutation =
                        self.agent_notifications
                            .agent_monitor
                            .notify(&agent_route, title, body);

                    Self::remove_native_notifications(&mutation.removed_notifications);

                    chrome_changed |= mutation.visible_changed;

                    self.process_native_notifications(cx);
                }
                _ => {}
            }
        }

        if session_changed {
            self.sync_session_memory(cx);

            self.sync_git_target(cx);
        }

        if chrome_changed {
            cx.notify();
        }
    }

    /// Open the directory editor for an existing normal workspace. Confirming
    /// replaces that workspace's directory list; the tabs and conversations
    /// already running keep the directories they started with.
    pub(crate) fn edit_workspace_dirs(
        &mut self,
        id: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(roots) = self.workspaces.roots_of(id).cloned() else {
            return;
        };

        let editor = cx.new(|cx| WorkspaceDirsEditor::new(Some(roots), cx));
        let shell = cx.entity();

        window.open_dialog(cx, move |dialog, window, cx| {
            workspace_dirs_dialog(dialog, &editor, &shell, id, window, cx)
        });
    }

    /// Adopt an edited directory list. Open Agent Tabs of this workspace pick
    /// the new list up for their next conversation; the one they are running
    /// keeps the snapshot it started with.
    fn replace_workspace_roots(
        &mut self,
        id: WorkspaceId,
        roots: WorkspaceRoots,
        cx: &mut Context<Self>,
    ) {
        self.workspaces.set_roots(id, roots);

        self.sync_agent_workspaces(id, cx);

        self.refresh_root_availability(cx);

        self.sync_session_memory(cx);

        cx.notify();
    }

    /// Hand the edited directory list to every Agent Tab of this workspace.
    /// A pane holds its configured list apart from the snapshot its running
    /// conversation was started with, so this reaches the next conversation
    /// without disturbing the one in flight.
    fn sync_agent_workspaces(&mut self, id: WorkspaceId, cx: &mut Context<Self>) {
        let workspace = agent_workspace(self.workspaces.roots_of(id));

        let Some(tabs) = self.workspaces.tabs_of(id) else {
            return;
        };

        let panes: Vec<_> = tabs
            .list()
            .items()
            .iter()
            .filter_map(|tab| tab.surface().agent().cloned())
            .collect();

        for pane in panes {
            pane.update(cx, |pane, cx| {
                pane.set_workspace(workspace.clone(), cx);
            });
        }
    }

    /// Re-check every directory the open workspaces name.
    pub(crate) fn refresh_root_availability(&mut self, cx: &mut Context<Self>) {
        let paths: Vec<String> = self
            .workspaces
            .summaries()
            .iter()
            .flat_map(|ws| iter::once(ws.cwd.clone()).chain(ws.additional_cwds.iter().cloned()))
            .filter(|path| !path.trim().is_empty())
            .collect();

        self.root_availability.refresh(paths, cx);
    }

    /// The active workspace's directories paired with their last known
    /// availability, primary first. The New Tab menu snapshots this as it
    /// opens instead of touching the filesystem while the pointer waits.
    pub(crate) fn active_root_availability(&self) -> Vec<(String, bool)> {
        self.workspaces
            .active_roots()
            .into_iter()
            .flat_map(|roots| roots.ordered())
            .map(|path| (path.to_string(), self.root_availability.is_available(path)))
            .collect()
    }

    pub(super) fn apply_agent_monitor_display_change(
        mutation: &MonitorMutation,
        cx: &mut Context<Self>,
    ) {
        Self::remove_native_notifications(&mutation.removed_notifications);

        if mutation.visible_changed {
            cx.notify();
        }
    }

    pub(super) fn register_agent_pane(&mut self, pane: &Entity<TerminalPane>, cx: &App) {
        self.agent_notifications.agent_monitor.register_route(
            pane.read(cx).agent_route().clone(),
            AgentActivityPolicy::ExpireAfterInactivity,
            time::Instant::now(),
        );
    }

    pub(super) fn register_agent_tab(&mut self, pane: &Entity<AgentPane>, cx: &App) {
        let Some(route) = pane.read(cx).agent_route(cx).cloned() else {
            return;
        };

        self.agent_notifications.agent_monitor.register_route(
            route,
            AgentActivityPolicy::ExplicitLifecycle,
            time::Instant::now(),
        );
    }

    pub(super) fn remove_agent_route(&mut self, route: &AgentRoute, cx: &mut Context<Self>) {
        let mutation = self.agent_notifications.agent_monitor.remove_route(route);

        Self::apply_agent_monitor_display_change(&mutation, cx);

        self.agent_notifications.reschedule_agent_timer(cx);
    }

    pub(super) fn remove_native_notifications(notifications: &[AgentNotification]) {
        for notification in notifications {
            let tag = notification.native_tag.clone();
            let group = notification.native_group.clone();

            thread::spawn(move || {
                let _ = remove_notification(&tag, &group);
            });
        }
    }

    pub(super) fn exact_window_active(window: &Window) -> bool {
        native_active_state(window).unwrap_or_else(|| window.is_window_active())
    }

    pub(super) fn acknowledge_notification(
        &mut self,
        route: &AgentRoute,
        notification_id: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        let mutation = self
            .agent_notifications
            .agent_monitor
            .acknowledge(route, notification_id);

        Self::apply_agent_monitor_display_change(&mutation, cx);

        mutation.visible_changed
    }

    pub(super) fn process_native_notifications(&mut self, cx: &mut Context<Self>) {
        let system_notifications_enabled = system_notification_enabled();

        let visible_route = self
            .window_active
            .then(|| self.active_agent_route(cx))
            .flatten();

        for notification in self
            .agent_notifications
            .agent_monitor
            .pending_native_notifications()
        {
            if !request_native_delivery(visible_route.as_ref(), &notification.route) {
                self.acknowledge_notification(&notification.route, &notification.id, cx);

                continue;
            }

            if !self
                .agent_notifications
                .agent_monitor
                .mark_native_requested(&notification.route, &notification.id)
            {
                continue;
            }

            if !system_notifications_enabled {
                continue;
            }

            let activation_url: String = (&CliAction::FocusNotification {
                route: notification.route.clone(),
                notification_id: notification.id.clone(),
            })
                .into();

            thread::spawn(move || {
                match show_notification(&NativeNotification {
                    title: notification.title,
                    body: notification.body,
                    activation_url,
                    tag: notification.native_tag,
                    group: notification.native_group,
                }) {
                    Ok(()) => {}
                    Err(error) => warn!("native notification failed: {error}"),
                }
            });
        }
    }

    pub(super) fn agent_routes_in_surface(surface: &TabSurface, cx: &App) -> Vec<AgentRoute> {
        let mut routes: Vec<_> = surface
            .leaves()
            .into_iter()
            .map(|(_, pane)| pane.read(cx).agent_route().clone())
            .collect();

        if let Some(session) = surface.agent_session() {
            routes.push(session.read(cx).agent_route().clone());
        }

        routes
    }

    fn owns_agent_route(&self, route: &AgentRoute, cx: &App) -> bool {
        self.workspaces.all_tabs().any(|tabs| {
            tabs.list().items().iter().any(|tab| {
                Self::agent_routes_in_surface(tab.surface(), cx)
                    .iter()
                    .any(|candidate| candidate == route)
            })
        })
    }

    fn locate_agent_route(&self, route: &AgentRoute, cx: &App) -> Option<AgentRouteLocation> {
        for (workspace_index, summary) in self.workspaces.summaries().iter().enumerate() {
            let tabs = self.workspaces.tabs_of(summary.id)?;

            for (tab_index, tab) in tabs.list().items().iter().enumerate() {
                if let Some(pane) = tab.surface().agent()
                    && tab
                        .surface()
                        .agent_session()
                        .is_some_and(|session| session.read(cx).agent_route() == route)
                {
                    return Some(AgentRouteLocation {
                        workspace_id: summary.id,
                        workspace_index,
                        tab_id: tab.id(),
                        tab_index,
                        target: AgentRouteTarget::Agent(pane.clone()),
                    });
                }

                for (pane_id, pane) in tab.surface().leaves() {
                    if pane.read(cx).agent_route() == route {
                        return Some(AgentRouteLocation {
                            workspace_id: summary.id,
                            workspace_index,
                            tab_id: tab.id(),
                            tab_index,
                            target: AgentRouteTarget::Terminal {
                                pane_id,
                                pane: pane.clone(),
                            },
                        });
                    }
                }
            }
        }

        None
    }

    pub(crate) fn focus_notification(
        &mut self,
        route: &AgentRoute,
        notification_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self
            .agent_notifications
            .agent_monitor
            .notification(route)
            .is_some_and(|notification| notification.id == notification_id && !notification.read)
        {
            return false;
        }

        let Some(location) = self.locate_agent_route(route, cx) else {
            return false;
        };

        self.workspaces
            .list_mut()
            .activate(location.workspace_index);

        debug_assert_eq!(self.workspaces.list().active_id(), location.workspace_id);

        self.workspaces
            .active_tabs_mut()
            .list_mut()
            .activate(location.tab_index);

        debug_assert_eq!(
            self.workspaces.active_tabs().list().active_id(),
            location.tab_id
        );

        window.activate_window();

        match location.target {
            AgentRouteTarget::Terminal { pane_id, pane } => {
                self.workspaces
                    .active_tabs_mut()
                    .active_mut()
                    .live_mut()
                    .tree_mut()
                    .set_focused(pane_id);

                let handle = pane.read(cx).focus.clone();

                window.focus(&handle, cx);
            }
            AgentRouteTarget::Agent(pane) => {
                pane.update(cx, |pane, cx| pane.focus(window, cx));
            }
        }

        self.on_active_tab_changed(window, cx);

        self.acknowledge_notification(route, notification_id, cx);

        true
    }

    pub(crate) fn on_agent_event(&mut self, event: AgentEvent, cx: &mut Context<Self>) -> bool {
        if !self.owns_agent_route(&event.route, cx) {
            return false;
        }

        let mutation = self
            .agent_notifications
            .agent_monitor
            .apply(event, time::Instant::now());

        Self::apply_agent_monitor_display_change(&mutation, cx);

        self.agent_notifications.reschedule_agent_timer(cx);

        self.process_native_notifications(cx);

        true
    }

    fn process_due_agent_deadlines(&mut self, generation: u64, cx: &mut Context<Self>) {
        let Some(mutation) = self.agent_notifications.process_due(generation) else {
            return;
        };

        Self::apply_agent_monitor_display_change(&mutation, cx);

        self.agent_notifications.reschedule_agent_timer(cx);

        self.process_native_notifications(cx);
    }

    /// The tab holding this agent pane. A pane knows its route but not its
    /// tab, and the two are only related through the surface the tab owns.
    fn tab_for_agent_session(&self, session: &Entity<AgentSession>) -> Option<TabId> {
        self.workspaces
            .all_tabs()
            .flat_map(|tabs| tabs.list().items())
            .find(|tab| tab.surface().agent_session() == Some(session))
            .map(|tab| tab.id())
    }

    pub(crate) fn watch_agent_tab(pane: &Entity<AgentPane>, cx: &mut Context<Self>) {
        let Some(session) = pane.read(cx).agent_session() else {
            return;
        };

        cx.subscribe(&session, Self::on_agent_pane_event).detach();
    }

    fn on_agent_pane_event(
        &mut self,
        session: Entity<AgentSession>,
        event: &AgentPaneEvent,
        cx: &mut Context<Self>,
    ) {
        let route = session.read(cx).agent_route().clone();

        let mutation = match event {
            AgentPaneEvent::Lifecycle(event) if event.route == route => self
                .agent_notifications
                .agent_monitor
                .apply(event.clone(), time::Instant::now()),
            AgentPaneEvent::Lifecycle(_) => return,
            AgentPaneEvent::WorkflowActivity => {
                // Sticky: a finished run stays reachable, so the control
                // never goes away once it has appeared. The running count
                // is read at render time, so this only has to repaint.
                self.panels.note_workflow_seen();

                cx.notify();

                return;
            }
            AgentPaneEvent::BackgroundTaskActivity => {
                // Sticky: a finished child stays reachable, so the control
                // never goes away once it has appeared. The running count
                // is read at render time, so this only has to repaint the
                // title bar.
                self.panels
                    .note_background_task_seen(session.read(cx).background_task_count() > 0);

                cx.notify();

                return;
            }
            AgentPaneEvent::ResumeElsewhere { cwd, session_id } => {
                // Opening a tab needs a window, which an event
                // subscription has none of; the next render has one.
                self.agent_notifications.pending_agent_resume = Some(PendingAgentResume {
                    profile: session.read(cx).profile().clone(),
                    cwd: cwd.clone(),
                    session_id: session_id.clone(),
                });

                cx.notify();

                return;
            }
            AgentPaneEvent::TitleSuggested(title) => {
                // A user-authored rename outranks this, so a tab the user
                // has named keeps its name.
                if let Some(tab_id) = self.tab_for_agent_session(&session)
                    && let Some(tabs) = self.workspaces.tabs_for_tab_mut(tab_id)
                    && tabs.set_title(tab_id, title.clone())
                {
                    cx.notify();
                }

                return;
            }
            AgentPaneEvent::CloseRequested => {
                // Same reason as the resume above: closing a tab needs a
                // window, and the next render has one.
                self.agent_notifications.pending_agent_close = self.tab_for_agent_session(&session);

                cx.notify();

                return;
            }
            AgentPaneEvent::Interrupted => self
                .agent_notifications
                .agent_monitor
                .interrupt(&route, time::Instant::now()),
        };

        Self::apply_agent_monitor_display_change(&mutation, cx);

        self.agent_notifications.reschedule_agent_timer(cx);

        self.process_native_notifications(cx);
    }

    fn bind_actions(element: Div, cx: &mut Context<Self>) -> Div {
        #[cfg(windows)]
        let element = element.on_action(cx.listener(Self::on_new_remote_tab));

        element
            .on_action(cx.listener(Self::on_new_tab))
            .on_action(cx.listener(Self::on_close_tab))
            .on_action(cx.listener(Self::on_next_tab))
            .on_action(cx.listener(Self::on_prev_tab))
            .on_action(cx.listener(Self::on_new_workspace))
            .on_action(cx.listener(Self::on_next_workspace))
            .on_action(cx.listener(Self::on_prev_workspace))
            .on_action(cx.listener(Self::on_new_window))
            .on_action(cx.listener(Self::on_split_up))
            .on_action(cx.listener(Self::on_split_down))
            .on_action(cx.listener(Self::on_split_left))
            .on_action(cx.listener(Self::on_split_right))
            .on_action(cx.listener(Self::on_resize_pane_up))
            .on_action(cx.listener(Self::on_resize_pane_down))
            .on_action(cx.listener(Self::on_resize_pane_left))
            .on_action(cx.listener(Self::on_resize_pane_right))
            .on_action(cx.listener(Self::on_toggle_sidebar))
            .on_action(cx.listener(Self::on_toggle_git_sidebar))
            .on_action(cx.listener(Self::on_toggle_background_tasks))
            .on_action(cx.listener(Self::on_show_settings))
            .on_action(cx.listener(Self::on_new_agent_tab))
            .on_action(cx.listener(Self::on_new_team_tab))
    }

    fn render_title_bar(
        &mut self,
        tab_bar: impl IntoElement,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        // Vertical tabs move the strip into the sidebar, which leaves the
        // middle of the bar free to name the session on screen instead.
        let vertical_tabs =
            cx.global::<AppSettings>().config().appearance.tab_bar_style == TabBarStyle::Vertical;

        let leading_width = if cfg!(target_os = "macos") {
            (self.sidebar.width + ui::composition::FLOATING_SURFACE_SIDE_INSET
                - TITLE_BAR_LEADING_INSET)
                .max(0.0)
        } else {
            self.sidebar.width - ui::composition::FLOATING_SURFACE_SIDE_INSET
        };

        // Interactive chrome lives in the titlebar but is wrapped in
        // `occlude()`: that blocks the drag hitbox beneath it, so Windows
        // treats these regions as client (clickable) while the empty titlebar
        // space stays draggable. The wrappers must size to their content (no
        // `flex_1`), or they'd cover the whole bar and leave nothing to drag.
        // Add future titlebar buttons the same way.
        TitleBar::new()
            .h(px(TITLE_BAR_HEIGHT))
            .when(cfg!(target_os = "macos"), |bar| {
                bar.pl(px(TITLE_BAR_LEADING_INSET))
            })
            // The default X calls `remove_window()` directly (no
            // WM_CLOSE), skipping `on_window_should_close` — so the
            // shared close confirmation is handled here too.
            .on_close_window(cx.listener(|this, _, window, cx| {
                if this.confirm_window_close(window, cx) {
                    window.remove_window();
                }
            }))
            .child(
                title_bar_leading_region(leading_width)
                    .child(
                        div()
                            .flex_none()
                            .occlude()
                            .child(self.render_app_menu_button(cx)),
                    )
                    .child(
                        div()
                            .flex_none()
                            .w(px(1.))
                            .h(px(TITLE_BAR_DIVIDER_HEIGHT))
                            .bg(cx.theme().border),
                    )
                    .child(
                        div().flex_none().occlude().child(
                            Button::new("toggle-sidebar")
                                .ghost()
                                .size(px(TITLE_BAR_BUTTON))
                                .icon(if self.sidebar.collapsed {
                                    SideBarIcon::Expand
                                } else {
                                    SideBarIcon::Collapse
                                })
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.on_toggle_sidebar(&ToggleSidebar, window, cx)
                                })),
                        ),
                    )
                    // Anchored to the leading edge behind two fixed-width
                    // controls, so their screen position never moves with the
                    // sidebar width or the session title: repeated clicks can
                    // cycle through targets without the pointer chasing them.
                    // Both stay mounted and go disabled when there is nowhere
                    // to jump, which is what keeps that position stable.
                    .child(
                        div().flex_none().occlude().child(
                            Button::new("next-ready-tab")
                                .ghost()
                                .size(px(TITLE_BAR_BUTTON))
                                .icon(IconName::Bell)
                                .tooltip(t!("shell-next-ready-tab"))
                                .disabled(self.next_ready_tab(cx).is_none())
                                // The target is picked on the click rather
                                // than captured here, so a tab that went ready
                                // (or was closed) since this frame is still
                                // reached by the very next click.
                                .on_click(cx.listener(|this, _, window, cx| {
                                    let Some((workspace_index, tab_index)) =
                                        this.next_ready_tab(cx)
                                    else {
                                        return;
                                    };

                                    this.jump_to_tab(workspace_index, tab_index, window, cx);
                                })),
                        ),
                    )
                    .child(
                        div().flex_none().occlude().child(
                            Button::new("next-busy-tab")
                                .ghost()
                                .size(px(TITLE_BAR_BUTTON))
                                .icon(NextBusyTabIcon)
                                .tooltip(t!("shell-next-busy-tab"))
                                .disabled(self.next_busy_tab(cx).is_none())
                                .on_click(cx.listener(|this, _, window, cx| {
                                    let Some((workspace_index, tab_index)) = this.next_busy_tab(cx)
                                    else {
                                        return;
                                    };

                                    this.jump_to_tab(workspace_index, tab_index, window, cx);
                                })),
                        ),
                    )
                    // Absorbs the leftover width so the controls stay packed
                    // against the leading edge.
                    .child(div().flex_1().min_w_0()),
            )
            // The container keeps the title-bar drag area. Tabs and the
            // new-tab button block only their own bounds.
            .child(
                div()
                    .flex_1()
                    // A floor rather than `min_w_0`: without it flexbox drains
                    // this zero-basis column to nothing before squeezing its
                    // neighbours, and a zero-width strip cannot be scrolled
                    // back into view. The strip's own horizontal scroll takes
                    // over once the tabs no longer fit this width.
                    .min_w(px(TAB_STRIP_MIN_WIDTH))
                    .h_full()
                    .flex()
                    .items_center()
                    .min_w_0()
                    .map(|this| match vertical_tabs {
                        true => this.child(self.render_session_heading(cx)),
                        false => this.child(tab_bar),
                    }),
            )
            .child(
                h_flex()
                    // The window controls sit to the right of this group, so
                    // any width it concedes would be reclaimed by the tab
                    // strip and push them off the window.
                    .flex_none()
                    .child(div().occlude().child(self.chrome.git_status.clone()))
                    // The sidebar itself stays reachable through the
                    // `ToggleGitSidebar` action while the button is hidden.
                    .children(
                        cx.global::<AppSettings>()
                            .config()
                            .appearance
                            .show_git_status_on_title_bar
                            .then(|| {
                                div().occlude().child(
                                    Toggle::new("toggle-git-sidebar")
                                        .ghost()
                                        .checked(self.panels.shows(RightPanelKind::Git, cx))
                                        .icon(GitIcon)
                                        .on_click(cx.listener(|this, _: &bool, window, cx| {
                                            this.on_toggle_git_sidebar(
                                                &ToggleGitSidebar,
                                                window,
                                                cx,
                                            )
                                        })),
                                )
                            }),
                    )
                    // Each control gets its own occluding wrapper: a shared one
                    // would stack them, because the wrapper is a column.
                    // The workflow control stays out of the chrome until a run
                    // exists to look at.
                    .children(self.panels.workflows_seen().then(|| {
                        // Scoped to the active tab, because activating the
                        // control opens that tab's runs.
                        let running = self
                            .active_agent()
                            .map(|pane| pane.read(cx).running_workflow_agents())
                            .unwrap_or(0);

                        div()
                            .occlude()
                            .child(self.render_workflows_button(running, cx))
                    }))
                    // The background-task control stays out of the chrome until
                    // a tab has spawned a child to look at.
                    .children(if self.panels.background_tasks_seen() {
                        // The history flag is window-wide, so the active Agent
                        // gate keeps this Agent-only control off terminal tabs.
                        self.active_agent().map(|pane| {
                            // Scoped to the active tab, because activating the
                            // control opens that tab's children.
                            let running = pane.read(cx).running_background_tasks();

                            div()
                                .occlude()
                                .child(self.render_background_tasks_button(running, cx))
                        })
                    } else {
                        None
                    }),
            )
    }

    /// The leading control of the title bar. It carries the commands that have
    /// no chrome of their own; anything with a visible button of its own stays
    /// on that button rather than being listed here as well.
    fn render_app_menu_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let shell = cx.entity();

        ui::modern_dropdown(
            Button::new("app-menu")
                .ghost()
                .size(px(TITLE_BAR_BUTTON))
                .icon(IconName::Menu)
                .tooltip(t!("shell-app-menu"))
                .accessibility_label(t!("shell-app-menu")),
            move |menu, _, cx| app_menu(menu, &shell, cx),
        )
    }

    /// What the title bar names in the vertical tab-bar style, where the strip
    /// that would otherwise fill this space lives in the sidebar: the session
    /// on screen, and the branch its working directory is on.
    fn render_session_heading(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let branch = self
            .panels
            .git_model()
            .read(cx)
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.branch.clone());

        h_flex()
            .min_w_0()
            .gap(px(TITLE_BAR_HEADING_GAP))
            .items_center()
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(px(TITLE_BAR_HEADING_TEXT))
                    .text_color(cx.theme().muted_foreground)
                    .child(self.active_tab_title()),
            )
            .children(branch.map(|branch| {
                h_flex()
                    .flex_none()
                    .gap_1()
                    .items_center()
                    .rounded(px(TITLE_BAR_CHIP_RADIUS))
                    .px(px(TITLE_BAR_CHIP_PADDING_X))
                    .py(px(TITLE_BAR_CHIP_PADDING_Y))
                    .bg(cx.theme().muted)
                    .text_size(px(TITLE_BAR_CHIP_TEXT))
                    .text_color(cx.theme().muted_foreground)
                    .child(Icon::new(IconName::GitBranch).size(px(TITLE_BAR_CHIP_ICON)))
                    .child(branch)
            }))
    }

    /// Upper-right `Workflows` control, revealed once a run exists. It carries
    /// the number of agents running right now, which is the one thing about a
    /// workflow worth watching without opening the view; a run with nothing in
    /// flight shows the icon alone rather than a zero.
    fn render_workflows_button(&self, running: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let label = t!("workflows-running-agents", count = running).into_owned();

        Toggle::new("toggle-workflows")
            .ghost()
            .checked(self.panels.shows(RightPanelKind::Workflows, cx))
            // Matches the gap a Button puts between its icon and label; the
            // toggle centres its children without one.
            .gap_2()
            .icon(IconName::LayoutDashboard)
            .when(running > 0, |toggle| toggle.label(running.to_string()))
            .tooltip(label)
            .on_click(cx.listener(|this, _: &bool, window, cx| {
                this.on_toggle_workflows(&ToggleWorkflows, window, cx)
            }))
    }

    /// Upper-right `Background Tasks` control, revealed once a tab has spawned
    /// background work. It carries the number of tasks running right now; a
    /// session with none in flight shows the icon alone rather than a zero.
    /// The `ToggleBackgroundTasks` action still reaches the view while the
    /// control is hidden.
    fn render_background_tasks_button(
        &self,
        running: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let label = match running {
            0 => t!("tasks-background-title").to_string(),
            _ => t!("tasks-background-running-count", count = running).into_owned(),
        };

        Toggle::new("toggle-background-tasks")
            .ghost()
            .checked(self.panels.shows(RightPanelKind::BackgroundTasks, cx))
            .gap_2()
            .icon(IconName::Bot)
            .when(running > 0, |toggle| toggle.label(running.to_string()))
            .tooltip(label)
            .on_click(cx.listener(|this, _: &bool, window, cx| {
                this.on_toggle_background_tasks(&ToggleBackgroundTasks, window, cx)
            }))
    }

    /// Publish this window's session to the registry the app writes out on
    /// quit. Called from every path that changes what a restore would rebuild.
    pub(super) fn sync_session_memory(&self, cx: &mut Context<Shell>) {
        let session = session_state(&self.workspaces, self.doomed_workspace, cx);

        if let Some(entry) = cx.global_mut::<WindowRegistry>().get_mut(self.window_id) {
            entry.session = Some(session);
        }
    }

    pub(super) fn tab_strip_mut(&mut self) -> &mut TabStrip {
        &mut self.chrome.tab_strip
    }
}

fn close_last_workspace_dialog(
    dialog: Dialog,
    shell: &Entity<Shell>,
    id: WorkspaceId,
    message: &str,
    note: &Option<SharedString>,
) -> Dialog {
    let quit_shell = shell.clone();
    let replace_shell = shell.clone();
    let message = message.to_string();
    let note = note.clone();

    dialog
        .title(t!("shell-close-last-workspace-title"))
        .overlay_closable(false)
        .content(move |content, _, cx| {
            content.child(
                v_flex()
                    .gap_1()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(message.clone())
                    .children(note.clone().map(|note| div().font_bold().child(note))),
            )
        })
        .footer(
            DialogFooter::new()
                .child(
                    Button::new("replace-ws")
                        .min_w(DIALOG_BUTTON_MIN_WIDTH)
                        .label(t!("shell-close-new-default-workspace"))
                        .primary()
                        .on_click(move |_, window, cx| {
                            window.close_dialog(cx);

                            replace_shell
                                .update(cx, |this, cx| this.replace_last_workspace(id, window, cx));
                        }),
                )
                .child(
                    Button::new("quit-app")
                        .min_w(DIALOG_BUTTON_MIN_WIDTH)
                        .label(t!("shell-close-quit"))
                        .danger()
                        .on_click(move |_, window, cx| {
                            if !ui::settings::save_settings(window, cx) {
                                window.close_dialog(cx);

                                return;
                            }

                            quit_shell.update(cx, |this, cx| this.doom_workspace(id, cx));

                            cx.quit();
                        }),
                )
                .child(
                    DialogClose::new().child(
                        Button::new("keep-ws")
                            .min_w(DIALOG_BUTTON_MIN_WIDTH)
                            .label(t!("shell-close-cancel")),
                    ),
                ),
        )
}

pub(super) fn should_confirm_close(
    confirm: bool,
    warn: WarnBeforeTerminatingShell,
    child_process_count: &io::Result<usize>,
) -> bool {
    confirm
        || match child_process_count {
            Ok(count) => warn.should_warn(*count),
            Err(_) => warn != WarnBeforeTerminatingShell::Disabled,
        }
}

fn new_workspace_dialog(
    dialog: Dialog,
    name_input: &Entity<InputState>,
    dirs: &Entity<WorkspaceDirsEditor>,
    shell: &Entity<Shell>,
    window: &Window,
) -> Dialog {
    let name_input = name_input.clone();
    let dirs = dirs.clone();
    let content_name = name_input.clone();
    let content_dirs = dirs.clone();
    let shell = shell.clone();
    let margin_top = ((window.viewport_size().height - px(300.)) * 0.5).max(px(16.));

    dialog
        .title(t!("shell-workspace-new-title"))
        .overlay_closable(false)
        .margin_top(margin_top)
        .button_props(
            DialogButtonProps::default()
                .ok_text(t!("shell-workspace-create"))
                .cancel_text(t!("shell-workspace-cancel"))
                .show_cancel(true),
        )
        // Plain `Dialog` never renders `button_props` buttons (only
        // `AlertDialog` does), so the footer supplies them; the
        // wrappers dispatch Confirm/CancelDialog into on_ok/on_cancel.
        .footer(
            DialogFooter::new()
                .child(
                    DialogAction::new().child(
                        Button::new("create-ws")
                            .min_w(DIALOG_BUTTON_MIN_WIDTH)
                            .label(t!("shell-workspace-create"))
                            .primary(),
                    ),
                )
                .child(
                    DialogClose::new().child(
                        Button::new("cancel-ws")
                            .min_w(DIALOG_BUTTON_MIN_WIDTH)
                            .label(t!("shell-workspace-cancel")),
                    ),
                ),
        )
        .content(move |content, _, _| {
            content.child(
                v_flex()
                    .gap_2()
                    .child(div().text_sm().child(t!("shell-workspace-name-label")))
                    .child(Input::new(&content_name))
                    .child(content_dirs.clone()),
            )
        })
        .on_ok(move |_, window, cx| {
            let name = name_input.read(cx).value().trim().to_string();

            let Some(roots) = dirs.read(cx).roots().cloned() else {
                return false;
            };

            shell.update(cx, |this, cx| {
                this.create_workspace(name, roots, window, cx);
            });

            true
        })
}

/// Walk from just after `active` and wrap around, returning the first marked
/// slot. `active` itself is visited last, so a tab that gets marked while it is
/// the one on screen stays reachable instead of being skipped forever.
pub(super) fn next_marked_position(marks: &[bool], active: usize) -> Option<usize> {
    (1..=marks.len())
        .map(|offset| (active + offset) % marks.len())
        .find(|&index| marks[index])
}

fn workspace_dirs_dialog(
    dialog: Dialog,
    editor: &Entity<WorkspaceDirsEditor>,
    shell: &Entity<Shell>,
    id: WorkspaceId,
    window: &Window,
    cx: &App,
) -> Dialog {
    let editor = editor.clone();
    let content_editor = editor.clone();
    let shell = shell.clone();
    let margin_top = ((window.viewport_size().height - px(300.)) * 0.5).max(px(16.));

    dialog
        .title(t!("shell-workspace-edit-title"))
        .overlay_closable(false)
        .margin_top(margin_top)
        .button_props(
            DialogButtonProps::default()
                .ok_text(t!("shell-workspace-save"))
                .cancel_text(t!("shell-workspace-cancel"))
                .show_cancel(true),
        )
        .footer(
            DialogFooter::new()
                .w_full()
                .border_t_1()
                .border_color(cx.theme().border)
                .pt_4()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .line_height(relative(1.5))
                        .text_color(cx.theme().muted_foreground)
                        .child(t!("shell-workspace-dirs-applies-next")),
                )
                .child(
                    DialogAction::new().child(
                        Button::new("save-ws-dirs")
                            .min_w(DIALOG_BUTTON_MIN_WIDTH)
                            .label(t!("shell-workspace-save"))
                            .primary(),
                    ),
                )
                .child(
                    DialogClose::new().child(
                        Button::new("cancel-ws-dirs")
                            .min_w(DIALOG_BUTTON_MIN_WIDTH)
                            .label(t!("shell-workspace-cancel")),
                    ),
                ),
        )
        .content(move |content, _, _| content.child(content_editor.clone()))
        .on_ok(move |_, _, cx| {
            let Some(roots) = editor.read(cx).roots().cloned() else {
                return false;
            };

            shell.update(cx, |this, cx| this.replace_workspace_roots(id, roots, cx));

            true
        })
}

struct AgentRouteLocation {
    workspace_id: WorkspaceId,
    workspace_index: usize,
    tab_id: TabId,
    tab_index: usize,
    target: AgentRouteTarget,
}

enum AgentRouteTarget {
    Terminal {
        pane_id: PaneId,
        pane: Entity<TerminalPane>,
    },
    Agent(Entity<AgentPane>),
}

/// Width the tab strip keeps once the title bar runs out of room: about one
/// truncated tab plus the new-tab button, so the strip stays visible and its
/// horizontal scroll stays reachable at the window's minimum width.
pub(super) const TAB_STRIP_MIN_WIDTH: f32 = 120.0;

/// The bar is taller than the Fluent standard strip because it carries
/// controls and a session heading rather than a title alone. Window creation
/// reads it to re-anchor the macOS close/minimize/zoom buttons, which AppKit
/// would otherwise center in its own, shorter strip.
pub(crate) const TITLE_BAR_HEIGHT: f32 = 44.0;

const TITLE_BAR_LEADING_INSET: f32 = 80.0;

/// A leading-zone control: square, and spaced tightly enough that the group
/// reads as one cluster rather than as separate buttons.
const TITLE_BAR_BUTTON: f32 = 26.0;

const TITLE_BAR_BUTTON_GAP: f32 = 4.0;

// Four controls, the divider, four internal gaps, and a trailing gap must
// stay visible before the first tab, including at the sidebar's drag limit.
const TITLE_BAR_CONTROLS_WIDTH: f32 = 4.0 * TITLE_BAR_BUTTON + 1.0 + 5.0 * TITLE_BAR_BUTTON_GAP;

pub(crate) const MIN_SIDEBAR_WIDTH: f32 = if cfg!(target_os = "macos") {
    TITLE_BAR_LEADING_INSET + TITLE_BAR_CONTROLS_WIDTH - FLOATING_SURFACE_SIDE_INSET
} else {
    140.0
};

/// A hairline between the application menu and the layout controls beside it.
/// At 26px the two icon clusters would otherwise read as one undifferentiated
/// row, and the menu opens application-wide commands while its neighbours only
/// move the view around.
const TITLE_BAR_DIVIDER_HEIGHT: f32 = 18.0;

/// The session heading in the middle of the bar, and the branch chip beside
/// it. The chip is set smaller than the title because it qualifies the title
/// rather than competing with it.
const TITLE_BAR_HEADING_TEXT: f32 = 13.0;

const TITLE_BAR_HEADING_GAP: f32 = 10.0;
const TITLE_BAR_CHIP_TEXT: f32 = 12.0;
const TITLE_BAR_CHIP_RADIUS: f32 = 6.0;
const TITLE_BAR_CHIP_PADDING_X: f32 = 8.0;
const TITLE_BAR_CHIP_PADDING_Y: f32 = 2.0;
const TITLE_BAR_CHIP_ICON: f32 = 11.0;

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Safety net for any activation path that reaches a render without
        // passing `focus_active`: the visible tab must be live before anything
        // below reads the active pane.
        self.ensure_active_tab_live(window, cx);

        self.window_active = Self::exact_window_active(window);

        self.acknowledge_visible(window, false, cx);

        window.set_window_title(&self.active_tab_title());

        if self.chrome.needs_focus {
            self.chrome.needs_focus = false;

            self.on_active_tab_changed(window, cx);

            self.focus_active(window, cx);
        }

        if let Some(request) = self.agent_notifications.pending_agent_resume.take() {
            // The conversation ran in a directory of its own. Where a
            // workspace owns that directory, the reopened tab gets that
            // workspace's whole directory list; otherwise the conversation's
            // own directory is all this tab can honestly claim.
            let workspace =
                exact_match(&self.workspaces.summaries(), path::Path::new(&request.cwd))
                    .and_then(|id| self.workspaces.roots_of(id))
                    .map_or_else(
                        || AgentWorkspace::single(Some(request.cwd.clone())),
                        |roots| agent_workspace(Some(roots)),
                    );

            self.open_agent_tab_in(
                &request.profile,
                workspace,
                Some(RecoveryIdentity::new(
                    request.profile.kind,
                    request.session_id,
                )),
                window,
                cx,
            );
        }

        if let Some(tab) = self.agent_notifications.pending_agent_close.take() {
            self.request_close_tab(tab, window, cx);
        }

        self.process_native_notifications(cx);

        ui::sync_modern_menu(cx);

        // Any workspace/tab switch re-renders the shell, so this render-time
        // compare-and-set catches every switch path.
        self.sync_git_target(cx);

        self.panels.sync_agent_targets(self.active_agent(), cx);

        // The sidebar is always mounted so it can animate its width open/closed.
        let summaries = self.workspace_chrome(cx);

        // Vertical style folds the tab strip into the sidebar as child rows of
        // each workspace, leaving the title bar's strip slot empty.
        let vertical_tabs =
            cx.global::<AppSettings>().config().appearance.tab_bar_style == TabBarStyle::Vertical;

        let (unread_tabs, busy_agent_tabs) = self.tab_agent_indicators(cx);

        let sidebar_tabs: Vec<Vec<SidebarTab>> = match vertical_tabs {
            false => Vec::new(),
            true => summaries
                .iter()
                .map(|ws| {
                    let Some(tabs) = self.workspaces.tabs_of(ws.summary.id) else {
                        return Vec::new();
                    };

                    let active_id = tabs.list().active_id();

                    tabs.list()
                        .items()
                        .iter()
                        .map(|tab| SidebarTab {
                            id: tab.id(),
                            label: match tab.title().is_empty() {
                                true => SharedString::new_static("PowerShell"),
                                false => tab.title().to_string().into(),
                            },
                            // Every workspace keeps its own active tab, but
                            // only one of them is the tab on screen. Marking
                            // the others would put a selection highlight on
                            // every workspace's list at once.
                            active: ws.summary.active && tab.id() == active_id,
                            unread: unread_tabs.contains(&tab.id()),
                            busy: busy_agent_tabs.contains(&tab.id()),
                            bell: tab.bell(),
                            agent_kind: tab.surface().agent_kind(cx),
                            icon: tab.surface().icon(cx),
                            pending: matches!(tab.surface(), TabSurface::Pending(_)),
                            exited: tab.exited(),
                            progress: tab.progress(),
                            terminal: Self::tab_terminal_activity(tab, cx),
                        })
                        .collect()
                })
                .collect(),
        };

        let sidebar = self.sidebar.render(
            summaries,
            sidebar_tabs,
            &self.renames,
            SidebarUsage {
                daily: self.chrome.token_usage.clone(),
                quotas: self.chrome.agent_usage.clone(),
            },
            cx,
        );

        self.chrome.observe_root(window, cx);

        // Root stores opened dialogs but does not draw them; the app renders the
        // dialog overlay itself.
        let dialog_layer = Root::render_dialog_layer(window, cx);
        let notification_layer = Root::render_notification_layer(window, cx);
        let update_notification_layer = self.update_notifications.render(cx);

        // Scroll the newly active tab into view on any switch path.
        let active_id = self.workspaces.active_tabs().list().active_id();
        let active_index = self.workspaces.active_tabs().list().active_index();

        self.chrome
            .tab_strip
            .reveal_active(active_id, active_index, cx);

        let tab_bar = match vertical_tabs {
            true => div().into_any_element(),
            false => self.chrome.tab_strip.render(
                self.workspaces.active_tabs(),
                &unread_tabs,
                &busy_agent_tabs,
                &self.renames,
                cx,
            ),
        };

        self.apply_pending_ratios(cx);

        let pane_tree = self.render_active_tree(cx);

        let background_image = cx
            .global::<AppSettings>()
            .config()
            .appearance
            .background_image
            .clone()
            .map(|path| {
                let path: PathBuf = path.into();

                img(path)
                    .absolute()
                    .inset_0()
                    .size_full()
                    .object_fit(ObjectFit::Cover)
                    .opacity(ui::background_image_layer_opacity(cx))
            });

        let shell = div()
            .size_full()
            .relative()
            .overflow_hidden()
            // A context menu drawn in its own window never takes activation, so
            // that this window keeps its focused backdrop material — which also
            // means it never receives the press or the key that should dismiss
            // it. This window does. Capture phase, because the input still
            // belongs to whatever it was aimed at.
            .capture_any_mouse_down(|_, _, cx| ui::dismiss_modern_menu(cx))
            .capture_key_down(|event: &KeyDownEvent, _, cx| {
                // Capture phase, and propagation stops on anything the menu
                // used: while a menu is up its keys outrank the bindings of
                // whatever still holds focus underneath it.
                if dispatch_modern_menu_key(event, cx) {
                    cx.stop_propagation();
                }
            })
            // The window surface itself is never painted (gpui leaves it
            // white/transparent), and the chrome now has see-through regions —
            // the tab strip and the gutters around the terminal cards — so the
            // shell paints the chrome background across the whole window.
            // `apply_window_translucency` dims this color with the rest of the
            // chrome when window transparency is on.
            .bg(cx.theme().background)
            .flex()
            .flex_col()
            // All chrome inherits the configured UI font; terminal panes override it.
            .font(ui::font_with_default_fallback(
                cx.global::<AppSettings>()
                    .config()
                    .appearance
                    .ui_font
                    .clone(),
            ))
            .key_context("Shell");

        Self::bind_actions(shell, cx)
            .children(background_image)
            .child(self.render_title_bar(tab_bar, cx))
            .child(
                div()
                    .flex_1()
                    // Without this the row keeps `min-height: auto` and any
                    // child taller than the window stretches it, which pushes
                    // the bottom-anchored pane content below the viewport and
                    // reads as a blank main area.
                    .min_h_0()
                    .flex()
                    .flex_row()
                    .child(sidebar)
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .min_w_0()
                            .relative()
                            .overflow_hidden()
                            // Gutters only on the two sides that face other
                            // chrome; the surface runs flush into the window's
                            // right and bottom edges.
                            .pl(px(ui::composition::FLOATING_SURFACE_SIDE_INSET))
                            .pt(px(ui::composition::FLOATING_SURFACE_TOP_INSET))
                            .child(
                                floating_surface_card(cx)
                                    .id("main-floating-surface")
                                    .min_w_0()
                                    .relative()
                                    .child(pane_tree)
                                    // Notifications are anchored to the pane
                                    // viewport inside the clipped card.
                                    .children(notification_layer),
                            ),
                    )
                    .child(self.panels.panel().clone()),
            )
            .children(update_notification_layer)
            .children(dialog_layer)
    }
}

pub(super) fn title_bar_leading_region(width: f32) -> Div {
    // Sidebar alignment yields to the tab strip on narrow windows, while
    // the minimum width keeps every leading control reachable.
    h_flex()
        .w(px(width))
        .min_w(px(TITLE_BAR_CONTROLS_WIDTH))
        .flex_initial()
        .overflow_hidden()
        .gap(px(TITLE_BAR_BUTTON_GAP))
}

impl Focusable for Shell {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// Titlebar sidebar-toggle icons served by `crate::assets::AppAssets`.
enum SideBarIcon {
    Collapse,
    Expand,
}

impl IconNamed for SideBarIcon {
    fn path(self) -> SharedString {
        match self {
            Self::Collapse => "icons/side-bar-collapse.svg",
            Self::Expand => "icons/side-bar-expand.svg",
        }
        .into()
    }
}

/// The application menu: opening things, then the two application-wide
/// commands. Every entry here is reachable by keyboard as well, so the menu is
/// a place to find them rather than the only way to reach them.
fn app_menu(menu: ModernMenu, shell: &Entity<Shell>, _cx: &mut App) -> ModernMenu {
    let window_shell = shell.clone();
    let workspace_shell = shell.clone();
    let settings_shell = shell.clone();

    let menu = menu
        .item(t!("shell-menu-new-window"), move |window, cx| {
            window_shell.update(cx, |this, cx| {
                this.on_new_window(&NewWindow, window, cx);
            });
        })
        .icon(Icon::new(IconName::Frame))
        .item(t!("shell-workspace-new-title"), move |window, cx| {
            workspace_shell.update(cx, |this, cx| {
                this.on_new_workspace(&NewWorkspace, window, cx);
            });
        })
        .icon(Icon::new(IconName::Folder))
        .separator()
        .item(t!("shell-workspace-settings-title"), move |window, cx| {
            settings_shell.update(cx, |this, cx| {
                this.on_show_settings(&ShowSettings, window, cx);
            });
        })
        .icon(Icon::new(IconName::Settings));

    // Only a build that can replace itself offers to check.
    #[cfg(windows)]
    let menu = menu
        .item(t!("shell-menu-check-updates"), |_, cx| check(cx))
        .icon(Icon::new(IconName::ArrowDown));

    menu
}

struct GitIcon;

impl IconNamed for GitIcon {
    fn path(self) -> SharedString {
        "icons/git.svg".into()
    }
}

/// Titlebar busy-tab jump icon, backed by the project's `assets/icons/
/// circle-arrow-right.svg`. The arrow is what separates it from the busy
/// spinner drawn on the tabs themselves: this control navigates to that work
/// rather than reporting it.
struct NextBusyTabIcon;

impl IconNamed for NextBusyTabIcon {
    fn path(self) -> SharedString {
        "icons/circle-arrow-right.svg".into()
    }
}

/// Frame for the main terminal or Agent surface. The surface runs into the
/// window's right and bottom edges, so it is framed only where it actually
/// borders other chrome: a left edge against the sidebar gutter, a top edge
/// under the tab strip, and a single rounded corner between them. Drawing a
/// border or radius on the other two sides would trace a line just inside the
/// window frame.
fn floating_surface_card(cx: &App) -> Div {
    div()
        .size_full()
        .overflow_hidden()
        .border_l_1()
        .border_t_1()
        .border_color(cx.theme().sidebar_border)
        .rounded_tl(UI_RADIUS)
        .bg(cx.theme().background)
}

const PANE_RESIZE_STEP: Pixels = px(30.0);
