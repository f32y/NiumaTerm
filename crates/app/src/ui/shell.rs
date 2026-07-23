use std::rc::Rc;
use std::{collections, path, thread, time};

use dirs::home_dir;
use gpui::prelude::*;
use gpui::{
    AnyElement, App, Axis, Context, Entity, FocusHandle, Focusable, MouseDownEvent, ObjectFit,
    PathPromptOptions, Pixels, Render, SharedString, Task, Window, WindowBounds, WindowId, actions,
    div, img, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::dialog::{DialogAction, DialogButtonProps, DialogClose, DialogFooter};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::resizable::{
    PANEL_MIN_SIZE, ResizablePanelGroup, ResizableState, resizable_panel,
};
use gpui_component::{ActiveTheme, IconName, IconNamed, Root, TitleBar, WindowExt, h_flex, v_flex};
use nmt_agent_utils::{
    AgentEvent, AgentMonitor, AgentNotification, AgentRoute, agent_process, exact_window_is_active,
    request_native_delivery,
};
use nmt_config::get;
use nmt_config::local_state::WindowState;
use nmt_config::system::WarnBeforeTerminatingShell;
use nmt_platform::{
    NativeNotification, remove_notification, show_notification, system_notification_enabled,
};
use tracing::warn;
use windows_sys::Win32::Foundation::HWND;

use crate::cli::CliAction;
use crate::pane_tree::{PaneId, PaneNode, PaneTree, RemoveOutcome, SplitDirection, SplitOutcome};
use crate::tabs::{TabId, TabManager};
use crate::terminal::session::HostEvent;
use crate::terminal::view::{AgentInterrupted, TerminalPane};
use crate::ui;
use crate::ui::codex_usage::CodexUsageView;
use crate::ui::git_sidebar::GitSidebar;
use crate::ui::git_status::{GitStatusModel, GitStatusView};
use crate::ui::settings::{AppSettings, settings_view};
use crate::ui::tab_bar::TabStrip;
use crate::ui::token_usage::TokenUsageView;
use crate::ui::workspace_sidebar::{self, Sidebar};
use crate::window::{AppWindow, LastActiveWindow, ShellEntry, ShellRegistry, WindowRegistry};
use crate::workspace::{self, DEFAULT_WORKSPACE_NAME, WorkspaceId, WorkspaceManager, best_match};

/// A workspace cwd as a shell working directory: `None` for empty or the
/// legacy `"."` placeholder (shells then start in their default directory).
fn explicit_cwd(cwd: &str) -> Option<String> {
    let cwd = cwd.trim();
    (!cwd.is_empty() && cwd != ".").then(|| cwd.to_string())
}

const PANE_RESIZE_STEP: Pixels = px(30.0);

actions!(
    NiumaTerm,
    [
        NewTab,
        CloseTab,
        NextTab,
        PrevTab,
        NewWorkspace,
        NextWorkspace,
        PrevWorkspace,
        NewWindow,
        SplitUp,
        SplitDown,
        SplitLeft,
        SplitRight,
        ResizePaneUp,
        ResizePaneDown,
        ResizePaneLeft,
        ResizePaneRight,
        ToggleSidebar,
        ToggleGitSidebar,
        ShowSettings,
    ]
);

pub(crate) type TerminalPaneTree = PaneTree<Entity<TerminalPane>, Entity<ResizableState>>;

struct AgentRouteLocation {
    workspace_id: WorkspaceId,
    workspace_index: usize,
    tab_id: TabId,
    tab_index: usize,
    pane_id: PaneId,
    pane: Entity<TerminalPane>,
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
    /// Active Codex account rate limits, refreshed independently of terminals.
    codex_usage: Entity<CodexUsageView>,
    /// Shared git status poller feeding the titlebar indicator and sidebar.
    git_model: Entity<GitStatusModel>,
    /// Titlebar `+N -M` indicator (self-gating on its setting).
    git_status: Entity<GitStatusView>,
    /// Right-side git sidebar panel; always mounted so close can animate.
    git_sidebar: Entity<GitSidebar>,
    git_sidebar_open: bool,
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

        let restored = Self::restore_session(
            remembered_session,
            default_profile.clone(),
            &mut restore_next_id,
            cx,
        );

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
                for (_, pane) in tab.surface().leaves() {
                    agent_monitor.register_route(pane.read(cx).agent_route().clone(), now);
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
            codex_usage: cx.new(CodexUsageView::new),
            git_status: cx.new(|cx| GitStatusView::new(git_model.clone(), cx)),
            git_sidebar: cx.new(|cx| GitSidebar::new(git_model.clone(), cx)),
            git_model,
            git_sidebar_open: false,
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
        let cwd = self
            .active_pane()
            .read(cx)
            .tab_state()
            .cwd
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
        self.git_sidebar_open = !self.git_sidebar_open;

        let open = self.git_sidebar_open;

        self.git_sidebar
            .update(cx, |sidebar, cx| sidebar.set_open(open, cx));

        self.git_model.update(cx, |model, cx| {
            model.sidebar_open = open;
            if open {
                model.refresh(cx);
            }
        });

        cx.notify();
    }

    pub(crate) fn alloc_id(next_id: &mut u64) -> u64 {
        let id = *next_id;

        *next_id += 1;

        id
    }

    fn register_agent_pane(&mut self, pane: &Entity<TerminalPane>, cx: &App) {
        self.agent_monitor
            .register_route(pane.read(cx).agent_route().clone(), time::Instant::now());
    }

    fn remove_agent_route(&mut self, route: &AgentRoute, cx: &mut Context<Self>) {
        let mutation = self.agent_monitor.remove_route(route);

        Self::remove_native_notifications(&mutation.removed_notifications);

        if mutation.visible_changed {
            cx.notify();
        }

        self.reschedule_agent_timer(cx);
    }

    fn remove_native_notifications(notifications: &[AgentNotification]) {
        for notification in notifications {
            let tag = notification.native_tag.clone();
            let group = notification.native_group.clone();

            thread::spawn(move || {
                let _ = remove_notification(&tag, &group);
            });
        }
    }

    fn exact_window_active(window: &Window) -> bool {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, IsIconic};

        let Ok(handle) = HasWindowHandle::window_handle(window) else {
            return false;
        };

        let RawWindowHandle::Win32(handle) = handle.as_raw() else {
            return false;
        };

        let hwnd = handle.hwnd.get() as HWND;
        let foreground = unsafe { GetForegroundWindow() };
        let gpui_active = window.is_window_active();
        let foreground_matches = foreground == hwnd;
        let minimized = unsafe { IsIconic(hwnd) } != 0;

        exact_window_is_active(gpui_active, foreground_matches, minimized)
    }

    fn acknowledge_notification(
        &mut self,
        route: &AgentRoute,
        notification_id: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        let mutation = self.agent_monitor.acknowledge(route, notification_id);

        Self::remove_native_notifications(&mutation.removed_notifications);

        if mutation.visible_changed {
            cx.notify();
        }

        mutation.visible_changed
    }

    fn process_native_notifications(&mut self, cx: &mut Context<Self>) {
        let system_notifications_enabled = system_notification_enabled();

        let visible_route = self
            .window_active
            .then(|| self.active_pane().read(cx).agent_route().clone());
        for notification in self.agent_monitor.pending_native_notifications() {
            if !request_native_delivery(visible_route.as_ref(), &notification.route) {
                self.acknowledge_notification(&notification.route, &notification.id, cx);

                continue;
            }

            if !self
                .agent_monitor
                .mark_native_requested(&notification.route, &notification.id)
            {
                continue;
            }

            if !system_notifications_enabled {
                continue;
            }

            let activation_url = CliAction::FocusNotification {
                route: notification.route.clone(),
                notification_id: notification.id.clone(),
            }
            .to_url();

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

    fn agent_routes_in_tree(tree: &TerminalPaneTree, cx: &App) -> Vec<AgentRoute> {
        tree.leaves()
            .into_iter()
            .map(|(_, pane)| pane.read(cx).agent_route().clone())
            .collect()
    }

    fn owns_agent_route(&self, route: &AgentRoute, cx: &App) -> bool {
        self.workspaces.all_tabs().any(|tabs| {
            tabs.tabs().iter().any(|tab| {
                tab.surface()
                    .leaves()
                    .into_iter()
                    .any(|(_, pane)| pane.read(cx).agent_route() == route)
            })
        })
    }

    fn locate_agent_route(&self, route: &AgentRoute, cx: &App) -> Option<AgentRouteLocation> {
        for (workspace_index, summary) in self.workspaces.summaries().iter().enumerate() {
            let tabs = self.workspaces.tabs_of(summary.id)?;

            for (tab_index, tab) in tabs.tabs().iter().enumerate() {
                for (pane_id, pane) in tab.surface().leaves() {
                    if pane.read(cx).agent_route() == route {
                        return Some(AgentRouteLocation {
                            workspace_id: summary.id,
                            workspace_index,
                            tab_id: tab.id(),
                            tab_index,
                            pane_id,
                            pane: pane.clone(),
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
            .agent_monitor
            .notification(route)
            .is_some_and(|notification| notification.id == notification_id && !notification.read)
        {
            return false;
        }

        let Some(location) = self.locate_agent_route(route, cx) else {
            return false;
        };

        self.workspaces.activate(location.workspace_index);

        debug_assert_eq!(self.workspaces.active_id(), location.workspace_id);

        self.workspaces
            .active_tabs_mut()
            .activate(location.tab_index);

        debug_assert_eq!(self.workspaces.active_tabs().active_id(), location.tab_id);

        self.workspaces
            .active_tabs_mut()
            .active_mut()
            .set_focused(location.pane_id);

        window.activate_window();

        let handle = location.pane.read(cx).focus.clone();

        window.focus(&handle, cx);

        self.acknowledge_notification(route, notification_id, cx);

        true
    }

    pub(crate) fn apply_agent_event(&mut self, event: AgentEvent, cx: &mut Context<Self>) -> bool {
        if !self.owns_agent_route(&event.route, cx) {
            return false;
        }

        let mutation = self.agent_monitor.apply(event, time::Instant::now());

        Self::remove_native_notifications(&mutation.removed_notifications);

        if mutation.visible_changed {
            cx.notify();
        }

        self.reschedule_agent_timer(cx);

        self.process_native_notifications(cx);

        true
    }

    fn reschedule_agent_timer(&mut self, cx: &mut Context<Self>) {
        self.agent_timer_generation = self.agent_timer_generation.wrapping_add(1);

        let generation = self.agent_timer_generation;

        let Some(deadline) = self.agent_monitor.next_deadline() else {
            return;
        };

        let delay = deadline.saturating_duration_since(time::Instant::now());

        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;

            let _ = this.update(cx, |this, cx| {
                if this.agent_timer_generation != generation {
                    return;
                }

                let mutation = this.agent_monitor.process_due(time::Instant::now());

                Self::remove_native_notifications(&mutation.removed_notifications);

                if mutation.visible_changed {
                    cx.notify();
                }

                this.reschedule_agent_timer(cx);
                this.process_native_notifications(cx);
            });
        })
        .detach();
    }

    fn default_profile(cx: &Context<Self>) -> (Option<String>, Vec<String>) {
        cx.global::<AppSettings>().default_profile_command()
    }

    /// Open a new window with a fresh default session, offset from this one so
    /// the two don't exactly overlap.
    fn on_new_window(&mut self, _: &NewWindow, window: &mut Window, cx: &mut Context<Self>) {
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

    /// The active tab's focused terminal pane entity.
    pub(crate) fn active_pane(&self) -> Entity<TerminalPane> {
        self.workspaces
            .active_tabs()
            .active()
            .focused_pane()
            .clone()
    }

    fn sync_active_terminal_title(&mut self, cx: &App) {
        let title = self.active_pane().read(cx).terminal_title();
        let tabs = self.workspaces.active_tabs_mut();
        let tab_id = tabs.active_id();

        tabs.set_title(tab_id, title);
    }

    pub(crate) fn focus_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_active_terminal_title(cx);

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

        let route = self.active_pane().read(cx).agent_route().clone();

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
                .flat_map(|tab| tab.surface().leaves())
                .map(|(_, pane)| pane.read(cx).agent_route().clone())
                .collect();

            let projection = self.agent_monitor.project(&routes);

            summary.agent_status = projection.status;
            summary.unread_count = projection.unread_count;
            summary.latest_unread_text = projection.latest_unread_text;
        }
        summaries
    }

    fn unread_tabs(&self, cx: &App) -> collections::HashSet<TabId> {
        self.workspaces
            .active_tabs()
            .tabs()
            .iter()
            .filter_map(|tab| {
                let routes: Vec<_> = tab
                    .surface()
                    .leaves()
                    .into_iter()
                    .map(|(_, pane)| pane.read(cx).agent_route().clone())
                    .collect();

                (self.agent_monitor.project(&routes).unread_count > 0).then_some(tab.id())
            })
            .collect()
    }

    /// Observe a pane so its host events reach the shell pump even when the tab
    /// is not visible (the render-damage/host-event split from the design).
    pub(crate) fn watch_pane(pane: &Entity<TerminalPane>, cx: &mut Context<Self>) {
        cx.observe(pane, |this, pane, cx| this.pump_pane(pane, cx))
            .detach();

        cx.subscribe(pane, |this, pane, _: &AgentInterrupted, cx| {
            let route = pane.read(cx).agent_route().clone();

            let mutation = this.agent_monitor.interrupt(&route, time::Instant::now());

            Self::remove_native_notifications(&mutation.removed_notifications);

            if mutation.visible_changed {
                cx.notify();
            }

            this.reschedule_agent_timer(cx);
        })
        .detach();
    }

    /// The id of the tab whose pane tree contains `pane_id`, searched across
    /// all workspaces (pane ids and tab ids come from the same counter but are
    /// no longer equal once a tab is split).
    fn tab_for_pane(&self, pane_id: PaneId) -> Option<TabId> {
        self.workspaces.find_tab_id(|tree| tree.contains(pane_id))
    }

    /// Host-event pump: drain one pane's events (applying its pane-side effects)
    /// and fold the chrome-visible ones into the owning tab.
    fn pump_pane(&mut self, pane: Entity<TerminalPane>, cx: &mut Context<Self>) {
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
                            .tab_manager_for(tab_id)
                            .and_then(|tabs| tabs.find(tab_id))
                            .is_some_and(|tab| tab.surface().focused() == pane_id)
                        && let Some(tabs) = self.workspaces.tab_manager_for_mut(tab_id)
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

                        if let Some(tabs) = self.workspaces.tab_manager_for_mut(tab_id) {
                            if let Some(tab) = tabs.find_mut(tab_id)
                                && !tab.surface().is_single_leaf()
                            {
                                removed = tab.surface_mut().remove(pane_id);
                            }
                            if removed.is_none() {
                                tabs.mark_exited(tab_id);
                            }
                        }

                        if let Some((pane, outcome)) = removed {
                            if let RemoveOutcome::RemovedFromSplit { state, index } = outcome {
                                state.update(cx, |state, cx| state.remove_panel(index, cx));
                            }

                            // Dropping the pane entity releases its surface and
                            // ConPTY, same as an explicit pane close.
                            drop(pane);

                            // Re-run focus_active on the next render (the pump
                            // has no Window).
                            self.needs_focus = true;

                            self.sync_session_memory(cx);
                        }
                        chrome_changed = true;
                    }
                }
                HostEvent::InteractiveState(_)
                | HostEvent::PromptBoundaryTrusted(_)
                | HostEvent::PromptStarted => chrome_changed = true,
                HostEvent::Cwd(_) => session_changed = true,
                HostEvent::Notification { title, body } => {
                    let mutation = self.agent_monitor.notify(&agent_route, title, body);

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

    pub(crate) fn on_new_tab(&mut self, _: &NewTab, window: &mut Window, cx: &mut Context<Self>) {
        let id = Self::alloc_id(&mut self.next_id);

        let default_profile = Self::default_profile(cx);
        let cwd = explicit_cwd(self.workspaces.active_cwd());
        let pane = Self::spawn_default_pane(cx, id, default_profile, cwd);

        self.register_agent_pane(&pane, cx);

        let title = pane.read(cx).profile_name().to_string();

        self.workspaces.active_tabs_mut().new_tab(
            PaneTree::new_leaf(PaneId(id), pane),
            TabId(id),
            title,
        );

        self.focus_active(window, cx);

        self.sync_session_memory(cx);

        cx.notify();
    }

    /// CLI `new_tab`: open `path` in the deepest workspace whose cwd contains
    /// it (new tab, shell starts in `path`), or in a fresh workspace when
    /// nothing matches, preserving the user's target.
    pub(crate) fn open_dir_tab(
        &mut self,
        path: &path::Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = path.display().to_string();

        let Some(ws_id) = best_match(&self.workspaces.summaries(), path) else {
            self.create_workspace(DEFAULT_WORKSPACE_NAME.into(), target, window, cx);
            return;
        };

        if let Some(index) = self
            .workspaces
            .summaries()
            .iter()
            .position(|ws| ws.id == ws_id)
        {
            self.workspaces.activate(index);
        }

        let id = Self::alloc_id(&mut self.next_id);
        let default_profile = Self::default_profile(cx);
        let pane = Self::spawn_default_pane(cx, id, default_profile, Some(target));

        self.register_agent_pane(&pane, cx);

        let title = pane.read(cx).profile_name().to_string();

        self.workspaces.active_tabs_mut().new_tab(
            PaneTree::new_leaf(PaneId(id), pane),
            TabId(id),
            title,
        );

        self.focus_active(window, cx);
        self.sync_session_memory(cx);

        cx.notify();
    }

    fn on_close_tab(&mut self, _: &CloseTab, window: &mut Window, cx: &mut Context<Self>) {
        // With the tab split, the close shortcut closes the focused pane; the
        // last remaining pane falls through to the tab-close cascade.
        if !self.workspaces.active_tabs().active().is_single_leaf() {
            self.request_close_pane(window, cx);
            return;
        }

        let id = self.workspaces.active_tabs().active_id();

        self.request_close_tab(id, window, cx);
    }

    /// Close the focused pane of the active (multi-pane) tab, with a confirm
    /// dialog first when its shell has running child processes.
    fn request_close_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.workspaces.active_tabs().active().focused();
        let pane = self.active_pane();
        let settings = cx.global::<AppSettings>();
        let count = if settings.manage_subprocess_job
            && settings.warn_before_terminating_shell != WarnBeforeTerminatingShell::Disabled
        {
            pane.read(cx).child_process_count()
        } else {
            0
        };

        if !settings.warn_before_terminating_shell.should_warn(count) {
            self.close_pane_now(id, window, cx);
            return;
        }

        let description = if count > 0 {
            format!(
                "{} in this pane. Closing the pane will terminate them.",
                Self::processes_running(count)
            )
        } else {
            "Closing the pane will terminate its shell.".to_string()
        };

        Self::open_close_confirm(
            window,
            cx,
            "Close this pane?",
            description,
            move |this, window, cx| this.close_pane_now(id, window, cx),
        );
    }

    fn close_pane_now(&mut self, id: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        let tree = self.workspaces.active_tabs_mut().active_mut();

        let Some((pane, outcome)) = tree.remove(id) else {
            return;
        };

        if let RemoveOutcome::RemovedFromSplit { state, index } = outcome {
            state.update(cx, |state, cx| state.remove_panel(index, cx));
        }

        let route = pane.read(cx).agent_route().clone();

        self.remove_agent_route(&route, cx);

        // Dropping the pane entity drops its surface, releasing the IO thread
        // and ConPTY handle (same Drop chain as a tab close).
        drop(pane);

        self.focus_active(window, cx);
        self.sync_session_memory(cx);

        cx.notify();
    }

    /// "1 child process is running" / "N child processes are running" — the
    /// lead-in of every close-confirmation description.
    fn processes_running(count: usize) -> String {
        if count == 1 {
            "1 child process is running".to_string()
        } else {
            format!("{count} child processes are running")
        }
    }

    /// Child processes running across every pane of workspace `id`.
    fn workspace_process_count(&self, id: WorkspaceId, cx: &App) -> usize {
        self.workspaces.tabs_of(id).map_or(0, |tabs| {
            tabs.tabs()
                .iter()
                .map(|tab| self.close_process_count(tab.surface(), cx))
                .sum()
        })
    }

    /// Shared scaffolding of every close-confirmation alert: title +
    /// description, OK runs `on_confirm` against this shell.
    fn open_close_confirm(
        window: &mut Window,
        cx: &mut Context<Self>,
        title: &'static str,
        description: String,
        on_confirm: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let shell = cx.entity();
        let on_confirm = Rc::new(on_confirm);

        window.open_alert_dialog(cx, move |alert, _, _| {
            let shell = shell.clone();
            let on_confirm = Rc::clone(&on_confirm);

            alert
                .confirm()
                .title(title)
                .description(description.clone())
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
    fn close_process_count(&self, tree: &TerminalPaneTree, cx: &App) -> usize {
        let settings = cx.global::<AppSettings>();

        if !settings.manage_subprocess_job
            || settings.warn_before_terminating_shell == WarnBeforeTerminatingShell::Disabled
        {
            return 0;
        }

        tree.leaves()
            .into_iter()
            .map(|(_, pane)| pane.read(cx).child_process_count())
            .sum()
    }

    /// Close the tab in the active workspace, with a confirm dialog first
    /// when the shell has running child processes. Closing the last tab
    /// closes its workspace too (confirmed first); on the last workspace
    /// that routes into the quit/replace/cancel dialog.
    pub(crate) fn request_close_tab(
        &mut self,
        id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let count = self
            .workspaces
            .active_tabs()
            .find(id)
            .map_or(0, |tab| self.close_process_count(tab.surface(), cx));

        if self.workspaces.active_tabs().len() == 1 {
            let ws_id = self.workspaces.active_id();

            if self.workspaces.len() == 1 {
                self.confirm_close_last_workspace(ws_id, window, cx);
                return;
            }

            let description = if count > 0 {
                format!(
                    "This is the last tab in this workspace, and {} in it. \
                     Closing it will terminate them and close the workspace.",
                    Self::processes_running(count)
                )
            } else {
                "This is the last tab in this workspace. Closing it also closes the workspace."
                    .to_string()
            };

            Self::open_close_confirm(
                window,
                cx,
                "Close the last tab?",
                description,
                move |this, window, cx| this.close_workspace_now(ws_id, window, cx),
            );

            return;
        }

        let warn = cx.global::<AppSettings>().warn_before_terminating_shell;

        if !warn.should_warn(count) {
            self.close_tab_now(id, window, cx);
            return;
        }

        let description = if count > 0 {
            format!(
                "{} in this tab. Closing the tab will terminate them.",
                Self::processes_running(count)
            )
        } else {
            "Closing the tab will terminate its shell.".to_string()
        };

        Self::open_close_confirm(
            window,
            cx,
            "Close this tab?",
            description,
            move |this, window, cx| this.close_tab_now(id, window, cx),
        );
    }

    fn close_tab_now(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        // `close` refuses the last tab and returns the removed pane entity;
        // dropping it drops the pane's surface and PTY.
        if let Some(tree) = self.workspaces.active_tabs_mut().close(id) {
            for route in Self::agent_routes_in_tree(&tree, cx) {
                self.remove_agent_route(&route, cx);
            }

            drop(tree);

            self.focus_active(window, cx);
            self.sync_session_memory(cx);

            cx.notify();
        }
    }

    /// Build an inline-rename input pre-filled with `current`, focused with the
    /// current name selected, and wired so Enter or blur (clicking anywhere
    /// else) invokes `finish` with commit = true. Escape is intercepted by the
    /// hosting row, which calls `finish` with commit = false.
    fn rename_input(
        current: String,
        finish: fn(&mut Self, bool, &mut Window, &mut Context<Self>),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        let input = cx.new(|cx| InputState::new(window, cx).default_value(current));

        cx.subscribe_in(
            &input,
            window,
            move |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                    finish(this, true, window, cx);
                }
            },
        )
        .detach();

        input.update(cx, |input, cx| {
            input.focus(window, cx);
            input.set_selected_range(0..input.text().len(), cx);
        });

        input
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

        let input = Self::rename_input(current, Self::finish_workspace_rename, window, cx);

        self.workspace_rename = Some((id, input));

        cx.notify();
    }

    /// End the in-flight workspace rename. Enter and blur commit the entered
    /// name (blank names are dropped by the manager); Escape reaches this
    /// with `commit` false, keeping the original name.
    pub(super) fn finish_workspace_rename(
        &mut self,
        commit: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((id, input)) = self.workspace_rename.take() else {
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
            .find(id)
            .map(|tab| tab.title().to_string())
        else {
            return;
        };

        let input = Self::rename_input(current, Self::finish_tab_rename, window, cx);

        self.tab_rename = Some((id, input));

        cx.notify();
    }

    /// End the in-flight tab rename; same semantics as the workspace rename
    /// (Enter/blur commit, Escape cancels, blank names are dropped).
    pub(super) fn finish_tab_rename(
        &mut self,
        commit: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((id, input)) = self.tab_rename.take() else {
            return;
        };

        if commit {
            let name = input.read(cx).value().trim().to_string();

            if !name.is_empty() {
                if let Some(tabs) = self.workspaces.tab_manager_for_mut(id) {
                    tabs.rename(id, name);

                    self.sync_session_memory(cx);
                }
            }
        }

        self.focus_active(window, cx);

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

        if self.workspaces.len() == 1 {
            self.confirm_close_last_workspace(id, window, cx);
            return;
        }

        let confirm = cx.global::<AppSettings>().confirm_before_closing_workspace;

        let count = self.workspace_process_count(id, cx);

        let warn = cx.global::<AppSettings>().warn_before_terminating_shell;

        if !confirm && !warn.should_warn(count) {
            self.close_workspace_now(id, window, cx);
            return;
        }

        let description = if count > 0 {
            format!(
                "{} in this workspace. Closing the workspace will terminate them.",
                Self::processes_running(count)
            )
        } else {
            "All tabs in this workspace will be closed and their shells terminated.".to_string()
        };

        Self::open_close_confirm(
            window,
            cx,
            "Close this workspace?",
            description,
            move |this, window, cx| this.close_workspace_now(id, window, cx),
        );
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

        let message = if count > 0 {
            format!(
                "This is the only workspace, and {} in it. Quit the app \
                 (terminating them), or replace it with a fresh workspace in \
                 your home directory?",
                Self::processes_running(count)
            )
        } else {
            "This is the only workspace. Quit the app, or replace it with a \
             fresh workspace in your home directory?"
                .to_string()
        };

        let shell = cx.entity();

        window.open_dialog(cx, move |dialog, _, _| {
            let quit_shell = shell.clone();
            let replace_shell = shell.clone();
            let message = message.clone();

            dialog
                .title("Close the last workspace?")
                .overlay_closable(false)
                .content(move |content, _, cx| {
                    content.child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(message.clone()),
                    )
                })
                .footer(
                    DialogFooter::new()
                        .child(DialogClose::new().child(Button::new("keep-ws").label("Cancel")))
                        .child(
                            Button::new("replace-ws")
                                .label("New Default Workspace")
                                .primary()
                                .on_click(move |_, window, cx| {
                                    window.close_dialog(cx);
                                    replace_shell.update(cx, |this, cx| {
                                        this.replace_last_workspace(id, window, cx)
                                    });
                                }),
                        )
                        .child(Button::new("quit-app").label("Quit").danger().on_click(
                            move |_, _, cx| {
                                quit_shell.update(cx, |this, cx| this.doom_workspace(id, cx));
                                cx.quit();
                            },
                        )),
                )
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
            .unwrap_or_default();

        self.create_workspace(String::new(), home, window, cx);

        self.close_workspace_now(id, window, cx);
    }

    /// True when the window may close right away. When child processes are
    /// running in any pane of any workspace, opens a confirm dialog instead
    /// (OK closes the window) and returns false. Reached from the titlebar X
    /// and the OS close request (Alt+F4, taskbar).
    pub(crate) fn confirm_window_close(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let count: usize = self
            .workspaces
            .all_tabs()
            .flat_map(|tabs| tabs.tabs())
            .map(|tab| self.close_process_count(tab.surface(), cx))
            .sum();

        let warn = cx.global::<AppSettings>().warn_before_terminating_shell;

        if !warn.should_warn(count) {
            return true;
        }

        let description = if count > 0 {
            format!(
                "{} in this window. Closing the window will terminate them.",
                Self::processes_running(count)
            )
        } else {
            "Closing the window will terminate its shells.".to_string()
        };

        // `remove_window` tears the window down directly (no WM_CLOSE
        // round-trip), so this dialog won't re-trigger.
        Self::open_close_confirm(
            window,
            cx,
            "Close this window?",
            description,
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
                tabs.tabs()
                    .iter()
                    .flat_map(|tab| Self::agent_routes_in_tree(tab.surface(), cx))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        if self.workspaces.close_workspace(id).is_some() {
            for route in routes {
                self.remove_agent_route(&route, cx);
            }

            self.focus_active(window, cx);

            self.sync_session_memory(cx);

            cx.notify();
        }
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

    fn on_next_tab(&mut self, _: &NextTab, window: &mut Window, cx: &mut Context<Self>) {
        self.workspaces.active_tabs_mut().focus_next();

        self.focus_active(window, cx);

        self.sync_session_memory(cx);

        cx.notify();
    }

    fn on_prev_tab(&mut self, _: &PrevTab, window: &mut Window, cx: &mut Context<Self>) {
        self.workspaces.active_tabs_mut().focus_prev();

        self.focus_active(window, cx);

        self.sync_session_memory(cx);

        cx.notify();
    }

    /// Open the new-workspace dialog with the shared default name and a working
    /// directory. Confirming creates the workspace; cancel creates nothing.
    pub(crate) fn on_new_workspace(
        &mut self,
        _: &NewWorkspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name_input = cx.new(|cx| {
            InputState::new(window, cx).default_value(DEFAULT_WORKSPACE_NAME.to_string())
        });

        let dir_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Working directory (required)"));

        let shell = cx.entity();

        window.open_dialog(cx, move |dialog, window, _| {
            let name_input = name_input.clone();
            let dir_input = dir_input.clone();
            let content_name = name_input.clone();
            let content_dir = dir_input.clone();
            let shell = shell.clone();
            let margin_top = ((window.viewport_size().height - px(300.)) * 0.5).max(px(16.));

            dialog
                .title("New Workspace")
                .overlay_closable(false)
                .margin_top(margin_top)
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("Create")
                        .cancel_text("Cancel")
                        .show_cancel(true),
                )
                // Plain `Dialog` never renders `button_props` buttons (only
                // `AlertDialog` does), so the footer supplies them; the
                // wrappers dispatch Confirm/CancelDialog into on_ok/on_cancel.
                .footer(
                    DialogFooter::new()
                        .child(DialogClose::new().child(Button::new("cancel-ws").label("Cancel")))
                        .child(
                            DialogAction::new()
                                .child(Button::new("create-ws").label("Create").primary()),
                        ),
                )
                .content(move |content, _, cx| {
                    let browse_dir = content_dir.clone();
                    content.child(
                        v_flex()
                            .gap_2()
                            .child(div().text_sm().child("Name"))
                            .child(Input::new(&content_name))
                            .child(div().text_sm().child("Working directory"))
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(div().flex_1().child(Input::new(&content_dir)))
                                    .child(
                                        Button::new("browse-workspace-dir")
                                            .label("Browse…")
                                            .on_click(move |_, window, cx| {
                                                let rx = cx.prompt_for_paths(PathPromptOptions {
                                                    files: false,
                                                    directories: true,
                                                    multiple: false,
                                                    prompt: None,
                                                    file_types: Vec::new(),
                                                });

                                                let dir_input = browse_dir.clone();

                                                window
                                                    .spawn(cx, async move |cx| {
                                                        if let Ok(Ok(Some(paths))) = rx.await
                                                            && let Some(path) = paths.first()
                                                        {
                                                            let value = path.display().to_string();

                                                            let _ = dir_input.update_in(
                                                                cx,
                                                                |state, window, cx| {
                                                                    state.set_value(
                                                                        value, window, cx,
                                                                    )
                                                                },
                                                            );
                                                        }
                                                    })
                                                    .detach();
                                            }),
                                    ),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Shells in this workspace start in this directory."),
                            ),
                    )
                })
                .on_ok(move |_, window, cx| {
                    let name = name_input.read(cx).value().trim().to_string();
                    let dir = dir_input.read(cx).value().trim().to_string();

                    if dir.is_empty() {
                        return false;
                    }

                    shell.update(cx, |this, cx| this.create_workspace(name, dir, window, cx));

                    true
                })
        });
    }

    /// Create a workspace named `name` (empty falls back to the shared default)
    /// whose shells start in `dir` (empty falls back to the default
    /// startup directory), seeded with one fresh tab, and activate it.
    fn create_workspace(
        &mut self,
        name: String,
        dir: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = if name.is_empty() {
            DEFAULT_WORKSPACE_NAME.to_string()
        } else {
            name
        };

        let cwd = explicit_cwd(&dir);
        let surface_id = Self::alloc_id(&mut self.next_id);
        let default_profile = Self::default_profile(cx);
        let pane = Self::spawn_default_pane(cx, surface_id, default_profile, cwd.clone());

        self.register_agent_pane(&pane, cx);

        let title = pane.read(cx).profile_name().to_string();

        let tabs = TabManager::new(
            PaneTree::new_leaf(PaneId(surface_id), pane),
            TabId(surface_id),
            title,
        );

        let ws_id = Self::alloc_id(&mut self.next_id);
        let ws_cwd = cwd.unwrap_or_else(|| ".".to_string());

        self.workspaces
            .new_workspace(tabs, WorkspaceId(ws_id), name, ws_cwd);

        self.focus_active(window, cx);

        self.sync_session_memory(cx);

        cx.notify();
    }

    fn on_next_workspace(
        &mut self,
        _: &NextWorkspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let len = self.workspaces.len();
        let next = (self.workspaces.active_index() + 1) % len;

        self.workspaces.activate(next);

        self.focus_active(window, cx);

        self.sync_session_memory(cx);

        cx.notify();
    }

    fn on_prev_workspace(
        &mut self,
        _: &PrevWorkspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let len = self.workspaces.len();
        let prev = (self.workspaces.active_index() + len - 1) % len;

        self.workspaces.activate(prev);

        self.focus_active(window, cx);

        self.sync_session_memory(cx);

        cx.notify();
    }

    fn on_split_up(&mut self, _: &SplitUp, window: &mut Window, cx: &mut Context<Self>) {
        self.split_pane(SplitDirection::Up, window, cx);
    }

    fn on_split_down(&mut self, _: &SplitDown, window: &mut Window, cx: &mut Context<Self>) {
        self.split_pane(SplitDirection::Down, window, cx);
    }

    fn on_split_left(&mut self, _: &SplitLeft, window: &mut Window, cx: &mut Context<Self>) {
        self.split_pane(SplitDirection::Left, window, cx);
    }

    fn on_split_right(&mut self, _: &SplitRight, window: &mut Window, cx: &mut Context<Self>) {
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
        let focused = self.active_pane();

        // Guard before mutating: the tree insert and the resizable-state
        // insert must both happen or neither (they are index-aligned).
        let has_room = focused.read(cx).content_size().is_none_or(|size| {
            let extent = match direction.axis() {
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

        let pane = Self::spawn_default_pane(cx, id, default_profile, cwd);

        self.register_agent_pane(&pane, cx);

        let tree = self.workspaces.active_tabs_mut().active_mut();

        match tree.split(PaneId(id), pane, direction, || {
            cx.new(|_| ResizableState::default())
        }) {
            SplitOutcome::Inserted {
                state,
                index,
                before,
            } => {
                // Halve the focused panel into the new sibling; siblings keep
                // their sizes.
                state.update(cx, |state, cx| state.split_panel(index, before, cx));
            }
            // A fresh two-child split lays out 50/50 on its own.
            SplitOutcome::Wrapped => {}
        }

        self.focus_active(window, cx);

        self.sync_session_memory(cx);

        cx.notify();
    }

    fn on_resize_pane_up(&mut self, _: &ResizePaneUp, window: &mut Window, cx: &mut Context<Self>) {
        self.resize_pane(SplitDirection::Up, window, cx);
    }

    fn on_resize_pane_down(
        &mut self,
        _: &ResizePaneDown,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.resize_pane(SplitDirection::Down, window, cx);
    }

    fn on_resize_pane_left(
        &mut self,
        _: &ResizePaneLeft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.resize_pane(SplitDirection::Left, window, cx);
    }

    fn on_resize_pane_right(
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
        let tree = self.workspaces.active_tabs().active();

        let Some((state, index, count)) = tree.resize_split(direction.axis()) else {
            return;
        };

        let Some(current) = state.read(cx).sizes().get(index).copied() else {
            return;
        };

        let grow = direction.positive() == (index + 1 < count);

        let target = if grow {
            current + PANE_RESIZE_STEP
        } else {
            current - PANE_RESIZE_STEP
        };

        state.update(cx, |state, cx| {
            state.resize_panel(index, target, window, cx)
        });

        self.sync_session_memory(cx);

        cx.notify();
    }

    /// Focus the pane `id` in the active tab (mouse click).
    pub(crate) fn focus_pane(&mut self, id: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        let tree = self.workspaces.active_tabs_mut().active_mut();

        if tree.focused() == id || !tree.set_focused(id) {
            return;
        }

        self.focus_active(window, cx);

        self.sync_session_memory(cx);

        cx.notify();
    }

    /// Apply saved split ratios once their groups have real bounds (the first
    /// visible frames after a session restore); cleared after applying.
    fn apply_pending_ratios(&mut self, cx: &mut Context<Self>) {
        let tree = self.workspaces.active_tabs_mut().active_mut();

        tree.for_each_split_mut(&mut |state, pending| {
            if let Some(ratios) = pending.take_if(|_| state.read(cx).has_bounds()) {
                state.update(cx, |state, cx| state.set_ratios(&ratios, cx));
            }
        });
    }

    /// The active tab's pane tree as nested resizable groups; a single-leaf
    /// tab renders its pane bare (no split chrome).
    fn render_active_tree(&self, cx: &mut Context<Self>) -> AnyElement {
        let tree = self.workspaces.active_tabs().active();

        let multi = !tree.is_single_leaf();

        Self::render_pane_node(tree.root(), tree.focused(), multi, cx)
    }

    fn render_pane_node(
        node: &PaneNode<Entity<TerminalPane>, Entity<ResizableState>>,
        focused: PaneId,
        multi: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match node {
            PaneNode::Leaf { id, pane, .. } => {
                let id = *id;

                // Every leaf renders as a rounded card framed by a 1px border,
                // separating the terminal surface from the window chrome. In
                // multi-pane layouts the border doubles as the focused-pane
                // highlight; it is present on every leaf so focus changes
                // don't shift layout.
                div()
                    .size_full()
                    .border_1()
                    .rounded(cx.theme().radius_lg)
                    .border_color(if multi && id == focused {
                        cx.theme().primary
                    } else {
                        cx.theme().border
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

impl Focusable for Shell {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// Titlebar sidebar-toggle icon, backed by the project's `assets/icons/
/// side-bar.svg` (served by `crate::assets::AppAssets`).
struct SideBarIcon;

impl IconNamed for SideBarIcon {
    fn path(self) -> SharedString {
        "icons/side-bar.svg".into()
    }
}

struct GitIcon;

impl IconNamed for GitIcon {
    fn path(self) -> SharedString {
        "icons/git.svg".into()
    }
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.window_active = Self::exact_window_active(window);

        self.acknowledge_visible(window, false, cx);

        window.set_window_title(&self.active_tab_title());

        if self.needs_focus {
            self.needs_focus = false;
            self.focus_active(window, cx);
        }

        self.process_native_notifications(cx);

        // Any workspace/tab switch re-renders the shell, so this render-time
        // compare-and-set catches every switch path.
        self.sync_git_target(cx);

        // The sidebar is always mounted so it can animate its width open/closed.
        let summaries = self.projected_workspace_summaries(cx);

        let sidebar = self.sidebar.render(
            summaries,
            self.workspace_rename.as_ref(),
            self.codex_usage.clone(),
            cx,
        );

        // Re-render the shell whenever the wrapping Root changes (dialog
        // open/close), since the shell draws the dialog layer.
        if !self.root_observed {
            if let Some(Some(root)) = window.root::<Root>() {
                cx.observe(&root, |_, _, cx| cx.notify()).detach();
                self.root_observed = true;
            }
        }

        // Root stores opened dialogs but does not draw them; the app renders the
        // dialog overlay itself.
        let dialog_layer = Root::render_dialog_layer(window, cx);
        let notification_layer = Root::render_notification_layer(window, cx);

        // Scroll the newly active tab into view on any switch path.
        let active_id = self.workspaces.active_tabs().active_id();
        let active_index = self.workspaces.active_tabs().active_index();

        self.tab_strip.reveal_active(active_id, active_index, cx);

        let unread_tabs = self.unread_tabs(cx);

        let tab_bar = self.tab_strip.render(
            self.workspaces.active_tabs(),
            &unread_tabs,
            self.tab_rename.as_ref(),
            cx,
        );

        self.apply_pending_ratios(cx);

        let pane_tree = self.render_active_tree(cx);

        let background_image = cx
            .global::<AppSettings>()
            .background_image
            .clone()
            .map(|path| {
                img(path::PathBuf::from(path))
                    .absolute()
                    .inset_0()
                    .size_full()
                    .object_fit(ObjectFit::Cover)
                    .opacity(ui::background_image_layer_opacity(cx))
            });

        div()
            .size_full()
            .relative()
            .overflow_hidden()
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
            .font_family(cx.global::<AppSettings>().ui_font_family.clone())
            .key_context("Shell")
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
            .on_action(cx.listener(Self::on_show_settings))
            .children(background_image)
            // Interactive chrome lives in the titlebar but is wrapped in
            // `occlude()`: that blocks the drag hitbox beneath it, so Windows
            // treats these regions as client (clickable) while the empty titlebar
            // space stays draggable. The wrappers must size to their content (no
            // `flex_1`), or they'd cover the whole bar and leave nothing to drag.
            // Add future titlebar buttons the same way.
            .child(
                TitleBar::new()
                    // The default X calls `remove_window()` directly (no
                    // WM_CLOSE), skipping `on_window_should_close` — so the
                    // running-processes confirmation is wired in here too.
                    .on_close_window(cx.listener(|this, _, window, cx| {
                        if this.confirm_window_close(window, cx) {
                            window.remove_window();
                        }
                    }))
                    // The bar spreads its children (justify_between), so the
                    // sidebar/settings buttons and token usage share one left group.
                    .child(
                        h_flex()
                            .child(
                                div().occlude().child(
                                    Button::new("toggle-sidebar")
                                        .ghost()
                                        .icon(SideBarIcon)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.on_toggle_sidebar(&ToggleSidebar, window, cx)
                                        })),
                                ),
                            )
                            .child(
                                div().occlude().child(
                                    Button::new("settings")
                                        .ghost()
                                        .icon(IconName::Settings)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.on_show_settings(&ShowSettings, window, cx)
                                        })),
                                ),
                            )
                            .children(
                                cx.global::<AppSettings>()
                                    .show_daily_token_usage
                                    .then(|| div().occlude().child(self.token_usage.clone())),
                            ),
                    )
                    .child(
                        h_flex()
                            .child(div().occlude().child(self.git_status.clone()))
                            .child(
                                div().occlude().child(
                                    Button::new("toggle-git-sidebar")
                                        .ghost()
                                        .icon(GitIcon)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.on_toggle_git_sidebar(
                                                &ToggleGitSidebar,
                                                window,
                                                cx,
                                            )
                                        })),
                                ),
                            ),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_row()
                    .child(sidebar)
                    .child(
                        div()
                            .flex_1()
                            // Don't let the tab strip's content width inflate
                            // this column past its flex share (min-width:auto
                            // would); the strip scrolls instead.
                            .min_w_0()
                            .flex()
                            .flex_col()
                            // Tab bar sits directly above the terminal (client
                            // area — no occlude needed outside the titlebar).
                            .child(tab_bar)
                            // `min_*_0`: the restored pane tree carries pixel
                            // flex-basis sizes whose sum sets a content min-size;
                            // without an explicit zero min the slot refuses to
                            // shrink.
                            .child(
                                div()
                                    .flex_1()
                                    .min_h_0()
                                    .min_w_0()
                                    .relative()
                                    .overflow_hidden()
                                    // Gutter floating the terminal card inside
                                    // the chrome; the tab strip above carries
                                    // its own 4px inset, so no top gap here.
                                    .px(px(6.))
                                    .pb(px(6.))
                                    .child(pane_tree)
                                    // Anchor notifications to the terminal viewport instead of
                                    // window chrome so its top margin remains layout-independent.
                                    .children(notification_layer),
                            ),
                    )
                    .child(self.git_sidebar.clone()),
            )
            .children(dialog_layer)
    }
}
