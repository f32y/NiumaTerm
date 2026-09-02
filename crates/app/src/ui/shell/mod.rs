mod actions;
mod agent_notifications;
mod close;
mod inline_rename;
mod panels;
mod panes;
mod pump;
mod render;
mod settings_workspace;
mod tab_presentation;
mod tab_surface;
mod tabs_open;
mod updates_layer;
mod workspace_dirs;
mod workspaces;

#[cfg(test)]
mod tests;

use std::rc::Rc;
use std::{collections, path, thread, time};

use dirs::home_dir;
use gpui::prelude::*;
use gpui::{
    Anchor, AnyElement, App, Axis, Context, Div, Entity, FocusHandle, Focusable, MouseDownEvent,
    ObjectFit, Pixels, Render, SharedString, Task, Window, WindowBounds, WindowId, div, img, px,
};
use gpui_component::button::{Button, ButtonVariants, Toggle, ToggleVariants};
use gpui_component::dialog::{
    DIALOG_BUTTON_MIN_WIDTH, DialogAction, DialogButtonProps, DialogClose, DialogFooter,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::notification::{Notification, NotificationType};
use gpui_component::progress::Progress;
use gpui_component::resizable::{
    PANEL_MIN_SIZE, ResizablePanelGroup, ResizableState, resizable_panel,
};
use gpui_component::setting::SettingsState;
use gpui_component::{
    ActiveTheme, Icon, IconName, IconNamed, Root, TitleBar, WindowExt, h_flex, v_flex,
};
use nmt_agent_utils::update::{ProviderKind, UpdatePhase};
use nmt_agent_utils::{
    AgentActivityPolicy, AgentEvent, AgentMonitor, AgentNotification, AgentRoute,
    AgentRuntimeStatus, AgentWorkspace, agent_process, request_native_delivery,
};
use nmt_app_agent::{AgentKind, AgentPane, AgentPaneEvent};
use nmt_app_terminal::session::HostEvent;
use nmt_app_terminal::view::{AgentInterrupted, TerminalPane};
use nmt_config::local_state::WindowState;
use nmt_config::system::WarnBeforeTerminatingShell;
use nmt_i18n::i18n;
use nmt_platform::{
    NativeNotification, remove_notification, show_notification, system_notification_enabled,
};
use tracing::warn;

use crate::agent_updates::{
    self as agent_updates, AgentUpdates, FocusedVisibleLifetime, NotificationPrimaryAction,
    NotificationProgress, UpdateNotificationTone, UpdateNotificationView,
};
use crate::agent_usage::AgentUsageView;
use crate::cli::CliAction;
use crate::pane_tree::{PaneId, PaneNode, PaneTree, RemoveOutcome, SplitDirection, SplitOutcome};
use crate::tabs::{CommandOutcome, Tab, TabId, TabManager};
use crate::ui::background_tasks::BackgroundTasksView;
use crate::ui::floating_surface;
use crate::ui::git_sidebar::GitSidebar;
use crate::ui::git_status::{GitStatusModel, GitStatusView};
use crate::ui::right_panel::{RightPanel, RightPanelKind};
use crate::ui::settings::{AgentProfile, AppSettings, TabBarStyle};
pub(crate) use crate::ui::shell::actions::{
    CloseTab, NewAgentTab, NewRemoteTab, NewTab, NewWindow, NewWorkspace, NextTab, NextWorkspace,
    PrevTab, PrevWorkspace, ResizePaneDown, ResizePaneLeft, ResizePaneRight, ResizePaneUp,
    ShowSettings, SplitDown, SplitLeft, SplitRight, SplitUp, ToggleBackgroundTasks,
    ToggleGitSidebar, ToggleSidebar, ToggleWorkflows,
};
#[cfg(test)]
use crate::ui::shell::close::{should_confirm_close, should_confirm_tab_close};
pub(super) use crate::ui::shell::inline_rename::{InlineRename, InlineRenameStyle};
use crate::ui::shell::panels::RightPanelController;
pub(super) use crate::ui::shell::tab_presentation::pending_tab_icon;
pub(crate) use crate::ui::shell::tab_surface::TabSurface;
use crate::ui::shell::updates_layer::UpdateNotificationLayer;
use crate::ui::shell::workspace_dirs::WorkspaceDirsEditor;
use crate::ui::tab_bar::TabStrip;
use crate::ui::token_usage::TokenUsageView;
use crate::ui::workflows::WorkflowsView;
use crate::ui::workspace_sidebar::{self, Sidebar, SidebarTab, SidebarUsage};
use crate::window::{AppWindow, LastActiveWindow, ShellEntry, ShellRegistry, WindowRegistry};
use crate::workspace::{
    self, ProgressTally, TerminalActivity, WorkspaceId, WorkspaceKind, WorkspaceManager,
    WorkspaceRoots, best_match, exact_match,
};
use crate::{remote, ui};

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
    agent_monitor: AgentMonitor,
    agent_timer_generation: u64,
    window_active: bool,
    /// Workspace-sidebar view state (collapse/expand + width) and its renderer.
    pub(super) sidebar: Sidebar,
    /// Tab-strip view state (scroll + active-tab reveal) and its renderer.
    pub(super) tab_strip: TabStrip,
    /// In-flight sidebar workspace rename: the item renders this input in
    /// place of its name. Enter or clicking anywhere else (blur) commits.
    pub(crate) workspace_rename: Option<(WorkspaceId, Entity<InputState>)>,
    /// In-flight tab rename in the tab bar; same lifecycle as
    /// `workspace_rename`.
    pub(crate) tab_rename: Option<(TabId, Entity<InputState>)>,
    /// Focus the active pane on the first render (the window root is `Root`, so
    /// initial focus can't be set from the app entry point).
    needs_focus: bool,
    /// A conversation from another directory, waiting for a render to open it
    /// in a tab rooted there. Event subscriptions carry no window, and opening
    /// a tab needs one.
    pending_agent_resume: Option<PendingAgentResume>,
    /// A tab whose agent asked to be closed, waiting for a render to close it.
    /// Closing a tab needs a window for the same reason opening one does.
    pending_agent_close: Option<TabId>,
    /// Whether we've started observing the wrapping `Root` (so dialog open/close
    /// re-renders the shell, which draws the dialog layer). Set on first render.
    root_observed: bool,
    /// Theme directory watcher, alive only while this shell's settings entry
    /// is open.
    theme_watcher: Option<Task<()>>,
    /// Selected page and search query of the settings surface. The shell owns
    /// it so switching to another workspace and back returns to the page the
    /// user left; the element state the component keeps by default would be
    /// dropped the frame the surface stops rendering.
    settings_state: Option<Entity<SettingsState>>,
    /// Whether the settings surface was the active tab at the last activation.
    /// Comparing against it turns every activation into an edge detector, so
    /// leaving the surface can flush the edits it left in the global.
    settings_was_active: bool,
    focus: FocusHandle,
    /// This shell's window in the `WindowRegistry`; all state writes target
    /// this entry.
    pub(crate) window_id: WindowId,
    /// Titlebar daily-token-usage widget; rendered only while the
    /// `show_daily_token_usage` setting is on. Rendered by the sidebar status
    /// cluster; the shell owns it so it outlives a sidebar collapse.
    token_usage: Entity<TokenUsageView>,
    /// Compact Codex and Claude rate limits, refreshed independently of terminals.
    agent_usage: Entity<AgentUsageView>,
    /// Titlebar `+N -M` indicator (self-gating on its setting).
    git_status: Entity<GitStatusView>,
    /// The right-side area and what points it at the active tab.
    panels: RightPanelController,
    /// Stable entities let each installation's card replace content in place
    /// without entering the transient Root notification lifecycle.
    /// On-screen provider-update notifications by key. Card entity, source
    /// view, and auto-hide clock live in one record so retiring a key cannot
    /// leave a stale sibling behind.
    update_notifications: UpdateNotificationLayer,
    /// Workspace directories the last background availability check could not
    /// reach, keyed by normalized path identity. A saved directory keeps its
    /// place in its workspace whether or not the filesystem can see it, so
    /// this drives presentation only.
    unavailable_roots: collections::HashSet<String>,
    /// Workspace excluded from session persistence: the user chose Quit in
    /// the close-last-workspace dialog, so it must not be restored on the
    /// next launch. Only set on the quit path — cancelling keeps everything.
    pub(crate) doomed_workspace: Option<WorkspaceId>,
}

impl Drop for Shell {
    fn drop(&mut self) {
        Self::remove_native_notifications(&self.agent_monitor.notifications());
    }
}

impl Shell {
    pub(crate) fn agent_panes(&self) -> Vec<Entity<AgentPane>> {
        self.workspaces
            .all_tabs()
            .flat_map(|tabs| tabs.tabs())
            .filter_map(|tab| tab.surface().agent().cloned())
            .collect()
    }

    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Repaint shell chrome when settings change.
        cx.observe_global_in::<AppSettings>(window, |_this, window, cx| {
            let _ = window;
            cx.notify();
        })
        .detach();

        // Stash the window geometry on every move/resize; main.rs flushes it
        // to local_state.toml on quit. Fires for both, and the Maximized
        // variant carries the restore bounds. Scan-and-update only: a stale
        // event after the window's entry is removed is a no-op.
        let window_id = window.window_handle().window_id();

        cx.observe_window_bounds(window, |_, window, cx| {
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
        })
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

        cx.observe_window_activation(window, |this, window, cx| {
            this.window_active = Self::exact_window_active(window);
            if this.window_active {
                cx.global_mut::<LastActiveWindow>().0 = Some(this.window_id);
                this.acknowledge_visible(window, true, cx);
            } else {
                // A context menu drawn in its own window never takes activation,
                // so it has none of its own to lose. This window losing it is
                // what says the user has moved on from the menu.
                ui::dismiss_modern_menu(cx);
            }
            this.process_native_notifications(cx);
            cx.notify();
        })
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
            .unwrap_or(workspace_sidebar::SIDEBAR_WIDTH);

        let mut restore_next_id = 1;

        let restored = Self::restore_session(remembered_session, &mut restore_next_id, window, cx);

        let (workspaces, next_id) = if let Some(workspaces) = restored {
            (workspaces, restore_next_id)
        } else {
            let mut next_id = 1;
            let workspaces = Self::default_session(initial_cwd, default_profile, &mut next_id, cx);
            (workspaces, next_id)
        };

        let now = time::Instant::now();

        let mut agent_monitor = AgentMonitor::new(agent_process().process_instance());

        for tabs in workspaces.all_tabs() {
            for tab in tabs.tabs() {
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
            agent_monitor,
            agent_timer_generation: 0,
            window_active: Self::exact_window_active(window),
            sidebar: Sidebar::new(sidebar_width),
            tab_strip: TabStrip::new(),
            workspace_rename: None,
            tab_rename: None,
            needs_focus: true,
            pending_agent_resume: None,
            pending_agent_close: None,
            root_observed: false,
            theme_watcher: None,
            settings_state: None,
            settings_was_active: false,
            focus: cx.focus_handle(),
            window_id,
            token_usage: cx.new(TokenUsageView::new),
            agent_usage: cx.new(AgentUsageView::new),
            git_status: cx.new(|cx| GitStatusView::new(git_model.clone(), cx)),
            panels: {
                let git = cx.new(|cx| GitSidebar::new(git_model.clone(), cx));
                let tasks = cx.new(|_| BackgroundTasksView::new());
                let workflows = cx.new(|_| WorkflowsView::new());
                let panel = cx.new(|_| RightPanel::new(git, tasks, workflows));
                RightPanelController::new(panel, git_model)
            },
            update_notifications: UpdateNotificationLayer::default(),
            unavailable_roots: collections::HashSet::new(),
            doomed_workspace: None,
        };

        this.sync_session_memory(cx);
        this.refresh_root_availability(cx);

        this
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
            .map(|tree| tree.focused_pane().clone())
    }

    /// The active tab's agent view, when it is an agent tab.
    fn active_agent(&self) -> Option<Entity<AgentPane>> {
        self.workspaces.active_tabs().active().agent().cloned()
    }

    fn active_agent_route(&self, cx: &App) -> Option<AgentRoute> {
        self.active_agent()
            .map(|pane| pane.read(cx).agent_route().clone())
            .or_else(|| {
                self.try_active_pane()
                    .map(|pane| pane.read(cx).agent_route().clone())
            })
    }

    /// Spawn a still-pending (lazily-restored) active tab, then register its
    /// panes' agent routes — the startup registration sweep only saw tabs that
    /// were live at window creation.
    fn ensure_active_tab_live(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !Self::materialize_active_tab(&mut self.workspaces, &mut self.next_id, window, cx) {
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
            self.agent_monitor
                .register_route(route, activity_policy, now);
        }
    }

    fn sync_active_terminal_title(&mut self, cx: &App) {
        let Some(pane) = self.try_active_pane() else {
            return;
        };
        let title = pane.read(cx).terminal_title();
        let tabs = self.workspaces.active_tabs_mut();
        let tab_id = tabs.active_id();

        tabs.set_title(tab_id, title);
    }

    pub(crate) fn focus_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_active_tab_live(window, cx);

        let settings_active = self.workspaces.active_tabs().active().is_settings();

        // Settings edits only live in the global until something writes them
        // out. Switching to another workspace leaves the surface on screen
        // for an unbounded time, so treat the departure as a commit point
        // instead of holding the edits until the entry is closed.
        if self.settings_was_active && !settings_active {
            cx.global::<AppSettings>().save();
        }
        self.settings_was_active = settings_active;

        // The settings surface owns its inner focus (its search field and
        // controls), and it has no pane to hand the keyboard to, so focus
        // stops at the shell.
        if settings_active {
            window.focus(&self.focus, cx);

            return;
        }

        self.sync_active_terminal_title(cx);

        // Every activation path funnels through here, so this is the one place
        // that acknowledges the tab's bell and its last command's result.
        let tabs = self.workspaces.active_tabs_mut();
        if tabs.clear_active_bell() | tabs.clear_active_outcome() {
            cx.notify();
        }

        // Agent tabs focus their composer and acknowledge their own monitor
        // route just like a focused terminal pane.
        if let Some(agent) = self.active_agent() {
            agent.update(cx, |pane, cx| pane.focus(window, cx));
            self.acknowledge_visible(window, true, cx);
            return;
        }

        let handle = self.active_pane().read(cx).focus.clone();

        window.focus(&handle, cx);

        self.acknowledge_visible(window, true, cx);
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

    fn projected_workspace_summaries(&self, cx: &App) -> Vec<workspace::WorkspaceSummary> {
        let mut summaries = self.workspaces.summaries();
        for summary in &mut summaries {
            let routes: Vec<_> = self
                .workspaces
                .tabs_of(summary.id)
                .into_iter()
                .flat_map(|tabs| tabs.tabs())
                .flat_map(|tab| Self::agent_routes_in_surface(tab.surface(), cx))
                .collect();

            let projection = self.agent_monitor.project(&routes);

            summary.agent_status = projection.status;
            summary.unread_count = projection.unread_count;
            summary.latest_unread_text = projection.latest_unread_text;

            summary.terminal_activity = self
                .workspaces
                .tabs_of(summary.id)
                .into_iter()
                .flat_map(|tabs| tabs.tabs())
                .map(|tab| Self::tab_terminal_activity(tab, cx))
                .fold(TerminalActivity::Idle, TerminalActivity::merge);

            // The manager's tally covers what the tabs report over OSC 9;4;
            // an agent's task list lives inside a pane entity, which only a
            // reader holding the app context can reach.
            summary.progress = self
                .workspaces
                .tabs_of(summary.id)
                .into_iter()
                .flat_map(|tabs| tabs.tabs())
                .filter_map(|tab| tab.surface().agent())
                .filter_map(|pane| pane.read(cx).task_tally(cx))
                .map(|(done, total)| ProgressTally::tasks(done, total))
                .fold(summary.progress, ProgressTally::merge);
        }
        summaries
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

        for tab in self.workspaces.all_tabs().flat_map(TabManager::tabs) {
            let routes = Self::agent_routes_in_surface(tab.surface(), cx);
            let projection = self.agent_monitor.project(&routes);

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
        let tab = &tabs.tabs()[tabs.active_index()];

        let base = if tab.title().is_empty() {
            "PowerShell"
        } else {
            tab.title()
        };

        if tab.exited() {
            i18n("shell-tab-exited-title").replace("{title}", base)
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
}
