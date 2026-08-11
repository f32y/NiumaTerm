mod actions;
mod agent_notifications;
mod close;
mod panes;
mod pump;
mod render;
mod tab_surface;
mod tabs_open;
mod updates_layer;
mod workspaces;

#[cfg(test)]
mod tests;

use std::rc::Rc;
use std::{collections, path, thread, time};

use dirs::home_dir;
use gpui::prelude::*;
use gpui::{
    Anchor, AnyElement, App, Axis, Context, Div, Entity, FocusHandle, Focusable, MouseDownEvent,
    ObjectFit, PathPromptOptions, Pixels, Render, SharedString, Task, Window, WindowBounds,
    WindowId, div, img, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::dialog::{DialogAction, DialogButtonProps, DialogClose, DialogFooter};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::notification::{Notification, NotificationType};
use gpui_component::progress::Progress;
use gpui_component::resizable::{
    PANEL_MIN_SIZE, ResizablePanelGroup, ResizableState, resizable_panel,
};
use gpui_component::{
    ActiveTheme, Icon, IconName, IconNamed, Root, TitleBar, WindowExt, h_flex, v_flex,
};
use nmt_agent_utils::background_task::BackgroundTaskKey;
use nmt_agent_utils::update::{ProviderKind, UpdatePhase};
use nmt_agent_utils::{
    AgentEvent, AgentMonitor, AgentNotification, AgentRoute, AgentRuntimeStatus, agent_process,
    exact_window_is_active, request_native_delivery,
};
use nmt_config::get;
#[cfg(test)]
use nmt_config::local_state::TabState;
use nmt_config::local_state::WindowState;
use nmt_config::system::WarnBeforeTerminatingShell;
use nmt_platform::{
    NativeNotification, remove_notification, show_notification, system_notification_enabled,
};
use tracing::warn;
use windows_sys::Win32::Foundation::HWND;

use crate::agent_pane::updates::{
    self as agent_updates, AgentUpdates, FocusedVisibleLifetime, NotificationPrimaryAction,
    NotificationProgress, UpdateNotificationTone, UpdateNotificationView,
};
use crate::agent_pane::usage::AgentUsageView;
use crate::agent_pane::{AgentKind, AgentPane, AgentPaneEvent};
use crate::cli::CliAction;
use crate::pane_tree::{PaneId, PaneNode, PaneTree, RemoveOutcome, SplitDirection, SplitOutcome};
use crate::tabs::{TabId, TabManager};
use crate::terminal::session::HostEvent;
use crate::terminal::view::{AgentInterrupted, TerminalPane};
use crate::ui::background_tasks::BackgroundTasksView;
use crate::ui::floating_surface;
use crate::ui::git_sidebar::GitSidebar;
use crate::ui::git_status::{GitStatusModel, GitStatusView};
use crate::ui::right_panel::{RightPanel, RightPanelKind};
use crate::ui::settings::{AgentProfile, AppSettings, settings_view};
pub(crate) use crate::ui::shell::actions::{
    CloseTab, NewAgentTab, NewRemoteTab, NewTab, NewWindow, NewWorkspace, NextTab, NextWorkspace,
    PrevTab, PrevWorkspace, ResizePaneDown, ResizePaneLeft, ResizePaneRight, ResizePaneUp,
    ShowSettings, SplitDown, SplitLeft, SplitRight, SplitUp, ToggleBackgroundTasks,
    ToggleGitSidebar, ToggleSidebar,
};
#[cfg(test)]
use crate::ui::shell::close::{should_confirm_close, should_confirm_tab_close};
#[allow(unused_imports)]
pub(crate) use crate::ui::shell::tab_surface::{TabSurface, TerminalPaneTree};
use crate::ui::tab_bar::TabStrip;
use crate::ui::token_usage::TokenUsageView;
use crate::ui::workspace_sidebar::{self, Sidebar};
use crate::window::{AppWindow, LastActiveWindow, ShellEntry, ShellRegistry, WindowRegistry};
use crate::workspace::{
    self, DEFAULT_WORKSPACE_NAME, WorkspaceId, WorkspaceManager, best_match, exact_match,
};
use crate::{remote, ui};

/// A workspace cwd as a shell working directory: `None` for empty or the
/// legacy `"."` placeholder (shells then start in their default directory).
pub(super) fn explicit_cwd(cwd: &str) -> Option<String> {
    let cwd = cwd.trim();
    (!cwd.is_empty() && cwd != ".").then(|| cwd.to_string())
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
    /// Whether we've started observing the wrapping `Root` (so dialog open/close
    /// re-renders the shell, which draws the dialog layer). Set on first render.
    root_observed: bool,
    /// Theme directory watcher, alive only while this shell's settings dialog is open.
    theme_watcher: Option<Task<()>>,
    focus: FocusHandle,
    /// This shell's window in the `WindowRegistry`; all state writes target
    /// this entry.
    pub(crate) window_id: WindowId,
    /// Titlebar daily-token-usage widget; rendered only while the
    /// `show_daily_token_usage` setting is on.
    token_usage: Entity<TokenUsageView>,
    /// Compact Codex and Claude rate limits, refreshed independently of terminals.
    agent_usage: Entity<AgentUsageView>,
    /// Shared git status poller feeding the titlebar indicator and sidebar.
    git_model: Entity<GitStatusModel>,
    /// Titlebar `+N -M` indicator (self-gating on its setting).
    git_status: Entity<GitStatusView>,
    /// The single right-side area, shared by Git and `Background Tasks`;
    /// always mounted so close can animate.
    right_panel: Entity<RightPanel>,
    /// Highest task activity ordinal the user has already seen, per parent
    /// session. Kept per session so opening one tab's view cannot hide new
    /// activity in another tab.
    seen_task_activity: collections::HashMap<BackgroundTaskKey, u64>,
    /// Stable entities let each installation's card replace content in place
    /// without entering the transient Root notification lifecycle.
    update_notifications: collections::HashMap<String, Entity<Notification>>,
    update_notification_views: collections::HashMap<String, UpdateNotificationView>,
    update_terminal_elapsed: collections::HashMap<String, FocusedVisibleLifetime>,
    update_notification_timer_running: bool,
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
                for route in Self::agent_routes_in_surface(tab.surface(), cx) {
                    agent_monitor.register_route(route, now);
                }
            }
        }

        let git_model = cx.new(GitStatusModel::new);

        let this = Self {
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
            root_observed: false,
            theme_watcher: None,
            focus: cx.focus_handle(),
            window_id,
            token_usage: cx.new(TokenUsageView::new),
            agent_usage: cx.new(AgentUsageView::new),
            git_status: cx.new(|cx| GitStatusView::new(git_model.clone(), cx)),
            right_panel: {
                let git = cx.new(|cx| GitSidebar::new(git_model.clone(), cx));
                let tasks = cx.new(|_| BackgroundTasksView::new());
                cx.new(|_| RightPanel::new(git, tasks))
            },
            git_model,
            seen_task_activity: collections::HashMap::new(),
            update_notifications: collections::HashMap::new(),
            update_notification_views: collections::HashMap::new(),
            update_terminal_elapsed: collections::HashMap::new(),
            update_notification_timer_running: false,
            doomed_workspace: None,
        };

        this.sync_session_memory(cx);

        this
    }

    /// Centralized target-CWD sync: read the active pane's
    /// OSC7-tracked CWD (falling back to the configured working-dir) and
    /// hand it to the git model, which no-ops when unchanged. Called on
    /// every render and on `HostEvent::Cwd`, so no switch path is missed.
    fn sync_git_target(&self, cx: &mut Context<Self>) {
        // Agent tabs have no OSC7-tracking pane; the configured working dir
        // keeps the git indicator on something sensible.
        let cwd = self
            .try_active_pane()
            .and_then(|pane| pane.read(cx).tab_state().cwd)
            .or_else(|| get().working_dir.clone());

        self.git_model
            .update(cx, |model, cx| model.set_target_cwd(cwd, cx));
    }

    fn on_toggle_git_sidebar(
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

    fn on_toggle_background_tasks(
        &mut self,
        _: &ToggleBackgroundTasks,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The view is scoped to one parent session, so a pane without a
        // started or restored provider session has nothing to open.
        let Some(parent) = self.active_task_parent(cx) else {
            return;
        };
        let open = self.right_panel.update(cx, |panel, cx| {
            panel.select(RightPanelKind::BackgroundTasks, cx)
        });

        if open {
            self.sync_task_panel_target(cx);
            // Opening marks only this session's activity as seen.
            if let Some(activity) = self.active_task_activity(cx) {
                self.seen_task_activity.insert(parent, activity);
            }
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

    /// Provider-qualified parent session of the active pane, or `None` when it
    /// is a terminal, an unsupported provider, or an Agent tab whose session id
    /// is not established yet.
    fn active_task_parent(&self, cx: &App) -> Option<BackgroundTaskKey> {
        self.active_agent()?.read(cx).background_task_parent()
    }

    fn active_task_activity(&self, cx: &App) -> Option<u64> {
        Some(self.active_agent()?.read(cx).background_tasks()?.activity)
    }

    /// Point the view at the active Agent pane and close it when that pane
    /// stopped being a supported provider session.
    fn sync_task_panel_target(&mut self, cx: &mut Context<Self>) {
        let target = self
            .active_agent()
            .filter(|pane| pane.read(cx).background_task_parent().is_some());

        if target.is_none() {
            self.right_panel.update(cx, |panel, cx| {
                panel.close_if_showing(RightPanelKind::BackgroundTasks, cx)
            });
        }

        let handle = target.map(|pane| pane.downgrade());
        let tasks = self.right_panel.read(cx).tasks().clone();
        tasks.update(cx, |view, cx| view.set_target(handle, cx));
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

        let routes = Self::agent_routes_in_surface(self.workspaces.active_tabs().active(), cx);

        let now = time::Instant::now();

        for route in routes {
            self.agent_monitor.register_route(route, now);
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

        self.sync_active_terminal_title(cx);

        // Every activation path funnels through here, so this is the one place
        // that acknowledges the tab's bell.
        if self.workspaces.active_tabs_mut().clear_active_bell() {
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
        }
        summaries
    }

    /// Project each tab's routes once for the two chrome indicators. Busy is
    /// limited to the dedicated Agent surface; terminal progress continues to
    /// use OSC 9;4 and must not acquire a second activity signal.
    fn tab_agent_indicators(
        &self,
        cx: &App,
    ) -> (collections::HashSet<TabId>, collections::HashSet<TabId>) {
        let mut unread_tabs = collections::HashSet::new();
        let mut busy_agent_tabs = collections::HashSet::new();

        for tab in self.workspaces.active_tabs().tabs() {
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
            format!("{base} [exited]")
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

    /// Open the settings dialog as a gpui-component modal. `Root` blocks the
    /// background; on close we restore focus to the active input for the current
    /// input style. The body is the two-pane `Settings` view from `crate::settings`.
    ///
    /// Field edits mutate the `AppSettings` global live (for preview); the whole
    /// set is persisted once here, on close. Only the top-right close button
    /// closes the dialog — mask clicks and Escape are disabled so a stray click
    /// can't dismiss it.
    fn on_show_settings(&mut self, _: &ShowSettings, window: &mut Window, cx: &mut Context<Self>) {
        self.theme_watcher = ui::watch_themes(cx);

        // Sized as a fraction of the window, so a large window gets a
        // proportionally large dialog.
        let shell = cx.entity();

        window.open_dialog(cx, move |dialog, window, _cx| {
            let shell = shell.clone();

            dialog
                .title("Settings")
                .overlay_closable(false)
                .keyboard(false)
                .on_close(move |_, window, cx| {
                    cx.global::<AppSettings>().save();
                    // Pick up relay URL / token edits made in the dialog.
                    ui::settings::reconcile_remote_host(cx);

                    shell.update(cx, |this, cx| {
                        this.theme_watcher = None;

                        this.focus_active(window, cx);
                    });
                })
                .w(window.viewport_size().width * 0.7)
                .content(|content, window, cx| {
                    content
                        .h(window.viewport_size().height * 0.7)
                        .child(settings_view(cx))
                })
        });
    }
}
