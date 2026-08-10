mod actions;
mod agent_notifications;
mod close;
mod panes;
mod pump;
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
    Anchor, AnyElement, App, Axis, Context, Entity, FocusHandle, Focusable, MouseDownEvent,
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
use crate::ui::floating_surface;
use crate::ui::git_sidebar::GitSidebar;
use crate::ui::git_status::{GitStatusModel, GitStatusView};
use crate::ui::settings::{AgentProfile, AppSettings, settings_view};
pub(crate) use crate::ui::shell::actions::{
    CloseTab, NewAgentTab, NewRemoteTab, NewTab, NewWindow, NewWorkspace, NextTab, NextWorkspace,
    PrevTab, PrevWorkspace, ResizePaneDown, ResizePaneLeft, ResizePaneRight, ResizePaneUp,
    ShowSettings, SplitDown, SplitLeft, SplitRight, SplitUp, ToggleGitSidebar, ToggleSidebar,
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
    /// Right-side git sidebar panel; always mounted so close can animate.
    git_sidebar: Entity<GitSidebar>,
    git_sidebar_open: bool,
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
            git_sidebar: cx.new(|cx| GitSidebar::new(git_model.clone(), cx)),
            git_model,
            git_sidebar_open: false,
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
        // Safety net for any activation path that reaches a render without
        // passing `focus_active`: the visible tab must be live before anything
        // below reads the active pane.
        self.ensure_active_tab_live(window, cx);

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
            self.agent_usage.clone(),
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
        let update_notification_layer = self.render_update_notification_layer(cx);

        // Scroll the newly active tab into view on any switch path.
        let active_id = self.workspaces.active_tabs().active_id();
        let active_index = self.workspaces.active_tabs().active_index();

        self.tab_strip.reveal_active(active_id, active_index, cx);

        let (unread_tabs, busy_agent_tabs) = self.tab_agent_indicators(cx);

        let tab_bar = self.tab_strip.render(
            self.workspaces.active_tabs(),
            &unread_tabs,
            &busy_agent_tabs,
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
            .on_action(cx.listener(Self::on_new_remote_tab))
            .on_action(cx.listener(Self::on_new_agent_tab))
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
                    // shared close confirmation is handled here too.
                    .on_close_window(cx.listener(|this, _, window, cx| {
                        if this.confirm_window_close(window, cx) {
                            window.remove_window();
                        }
                    }))
                    .child(
                        h_flex()
                            .w(px(self.sidebar.width - floating_surface::SIDE_INSET))
                            .flex_none()
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
                    // The container keeps the title-bar drag area. Tabs and the
                    // new-tab button block only their own bounds.
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .flex()
                            .items_center()
                            .child(tab_bar),
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
                            .min_h_0()
                            .min_w_0()
                            .relative()
                            .overflow_hidden()
                            .px(px(floating_surface::SIDE_INSET))
                            .pt(px(floating_surface::TOP_INSET))
                            .pb(px(floating_surface::BOTTOM_INSET))
                            .child(
                                floating_surface::card(cx)
                                    .id("main-floating-surface")
                                    .min_w_0()
                                    .relative()
                                    .child(pane_tree)
                                    // Notifications are anchored to the pane
                                    // viewport inside the clipped card.
                                    .children(notification_layer),
                            ),
                    )
                    .child(self.git_sidebar.clone()),
            )
            .children(update_notification_layer)
            .children(dialog_layer)
    }
}
