pub(crate) use crate::ui::shell::actions::{
    CloseTab, NewAgentTab, NewTab, NewWindow, NewWorkspace, NextTab, NextWorkspace, PrevTab,
    PrevWorkspace, QuoteGitLine, ResizePaneDown, ResizePaneLeft, ResizePaneRight, ResizePaneUp,
    ReturnFromGit, ShowSettings, SplitDown, SplitLeft, SplitRight, SplitUp, ToggleBackgroundTasks,
    ToggleGitSidebar, ToggleSidebar, ToggleWorkflows,
};
pub(crate) use crate::ui::shell::tab_surface::TabSurface;

pub(super) use crate::ui::shell::inline_rename::{InlineRename, InlineRenameStyle};
pub(super) use crate::ui::shell::rename::InlineRenameSession;
pub(super) use crate::ui::shell::tab_presentation::pending_tab_icon;

pub(crate) mod tab_surface;

mod actions;
mod agent_notifications;
mod close_confirm;
mod inline_rename;
mod main_surface;
mod panels;
mod rename;
mod render;
mod tab_presentation;
mod updates_layer;
mod workspace_dirs;

#[cfg(test)]
mod hibernation_tests;
#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::sync::Arc;
use std::{collections, io, iter, mem, path, time};

use app::agent_tab::execution::{AgentSession, SessionOwner};
use app::agent_tab::orchestration::OrchestrationPane;
use app::agent_tab::team::{TeamPane, TeamRuntime};
use app::agent_tab::{AgentPane, AgentPaneEvent};
use app::design::SETTINGS_NAV_WIDTH_PX;
use app::remote_control::CloseTab as SheetClose;
use app::terminal_tab::session::HostEvent;
use app::terminal_tab::view::{AgentInterrupted, TerminalPane};
use dirs::home_dir;
use gpui::prelude::*;
use gpui::{
    AnyElement, AnyView, AnyWindowHandle, App, AppContext, Axis, Bounds, Context, Div, Entity,
    EntityId, FocusHandle, Focusable, Global, KeyDownEvent, ObjectFit, Pixels, PlatformDisplay,
    Render, SharedString, TitlebarOptions, WeakEntity, Window, WindowAppearance, WindowBounds,
    WindowDecorations, WindowHandle, WindowId, WindowOptions, div, img, point, px, size,
    transparent_black,
};
use gpui_component::modern_menu::dispatch_modern_menu_key;
use gpui_component::resizable::PANEL_MIN_SIZE;
use gpui_component::{ActiveTheme, Root, Theme as ComponentTheme, WindowExt};
use nmt_agent::chat::SessionSummary;
use nmt_agent::team::model::RoomId;
use nmt_agent::{
    AgentActivityPolicy, AgentEvent, AgentMonitor, AgentRoute, AgentRuntimeStatus, AgentWorkspace,
    agent_process,
};
use nmt_config::local_state::{WindowLocalState, WindowState};
use nmt_config::system::WarnBeforeTerminatingShell;
use nmt_config::{config_dir_path, get};
use nmt_platform::default_shell_name;
use nmt_platform::filesystem::path_identity;
use nmt_platform::window::native_active_state;
use nmt_remote::NetworkPty;
use nmt_remote::local_view::LocalView;
use nmt_remote_core::identity::DeviceId;
use nmt_remote_core::rpc::{SessionInfo, WorkspaceInfo};
use rust_i18n::t;
use tracing::warn;

use crate::agent_updates::AgentUpdates;
use crate::tabs::{Tab, TabId, TabManager};
use crate::ui;
use crate::ui::background_tasks::BackgroundTasksView;
use crate::ui::git_sidebar::GitSidebar;
use crate::ui::git_status::GitStatusModel;
use crate::ui::pane_tree::{PaneId, SplitDirection};
use crate::ui::persistence::{
    default_session, hibernate_tab, materialize_active_tab, materialize_tab, restore_session,
    session_state,
};
use crate::ui::platform_style::{Host, PlatformStyle as _};
use crate::ui::remote::{self, Remote};
use crate::ui::right_panel::{RightPanel, RightPanelKind};
use crate::ui::settings::{
    AgentProfile, AppSettings, SettingsSurface, TabBarStyle, settings_title,
};
use crate::ui::shell::actions::NewTeamTab;
use crate::ui::shell::agent_notifications::{
    AgentNotificationState, apply_monitor_display_change, remove_native_notifications,
};
use crate::ui::shell::close_confirm::{
    ask_about_unsaved_orchestrations, close_description, close_last_workspace_dialog,
    open_close_confirm, open_save_failed_close, should_confirm_close, unsaved_orchestrations,
};
use crate::ui::shell::main_surface::{floating_surface_card, surface_border, tab_surface_view};
use crate::ui::shell::panels::RightPanelController;
use crate::ui::shell::render::ShellChrome;
use crate::ui::shell::tab_surface::{AgentTab, GitTab};
use crate::ui::shell::updates_layer::UpdateNotificationLayer;
use crate::ui::shell::workspace_dirs::{
    RootAvailability, open_new_workspace_dialog, open_workspace_dirs_dialog,
};
use crate::ui::tab_bar::{TabStrip, VerticalTabList, WorkspaceTabs};
use crate::ui::terminal_launch::{spawn_default_pane, spawn_local_view_pane, spawn_remote_pane};
use crate::ui::terminal_layout::TerminalLayout;
use crate::ui::title_bar::{PanelToggle, TitleBarInputs};
use crate::ui::workflows::WorkflowsView;
use crate::ui::workspace_sidebar;
use crate::ui::workspace_sidebar::{Sidebar, SidebarUsage, WorkspaceChrome};
use crate::workspace::{
    ProgressTally, TerminalActivity, WorkspaceId, WorkspaceKind, WorkspaceManager, WorkspaceRoots,
    best_match, exact_match, workspace_display_label,
};

/// Open terminal windows in creation order, plus the last closed window's
/// state for saving on quit or restoring when the application is reopened.
#[derive(Default)]
pub(crate) struct WindowRegistry {
    windows: Vec<WindowEntry>,
    last_closed: Option<WindowLocalState>,
}

impl Global for WindowRegistry {}

/// A window's current state and the references used to reach its interface
/// share one entry so closing it cannot leave a stale dispatch target.
pub(crate) struct WindowEntry {
    pub(crate) handle: AnyWindowHandle,
    pub(crate) view: WeakEntity<AppWindow>,
    state: WindowLocalState,
}

impl WindowRegistry {
    fn register(
        &mut self,
        handle: AnyWindowHandle,
        view: WeakEntity<AppWindow>,
        state: WindowLocalState,
    ) {
        // A newly opened window replaces the retained state even when its
        // caller requested a fresh session instead of restoring the old one.
        self.last_closed = None;

        self.windows.push(WindowEntry {
            handle,
            view,
            state,
        });
    }

    pub(crate) fn dispatch(
        cx: &mut App,
        mut action: impl FnMut(AnyWindowHandle, &WeakEntity<AppWindow>, &mut App) -> bool,
    ) -> bool {
        // Actions can change the registry. Copy only the dispatch references
        // so they run without borrowing it or cloning session snapshots.
        let targets: Vec<_> = cx
            .global::<Self>()
            .windows
            .iter()
            .map(|entry| (entry.handle, entry.view.clone()))
            .collect();

        targets
            .iter()
            .any(|(handle, view)| action(*handle, view, cx))
    }

    pub(crate) fn windows(&self) -> &[WindowEntry] {
        &self.windows
    }

    pub(crate) fn states(&self) -> impl Iterator<Item = &WindowLocalState> {
        self.windows
            .iter()
            .map(|entry| &entry.state)
            .chain(self.last_closed.iter())
    }

    pub(crate) fn prioritized(&self, last: Option<WindowId>) -> impl Iterator<Item = &WindowEntry> {
        self.windows
            .iter()
            .find(|entry| Some(entry.handle.window_id()) == last)
            .into_iter()
            .chain(
                self.windows
                    .iter()
                    .rev()
                    .filter(move |entry| Some(entry.handle.window_id()) != last),
            )
    }

    pub(crate) fn get(&self, id: WindowId) -> Option<&WindowLocalState> {
        self.windows
            .iter()
            .find(|entry| entry.handle.window_id() == id)
            .map(|entry| &entry.state)
    }

    pub(crate) fn get_mut(&mut self, id: WindowId) -> Option<&mut WindowLocalState> {
        self.windows
            .iter_mut()
            .find(|entry| entry.handle.window_id() == id)
            .map(|entry| &mut entry.state)
    }

    pub(crate) fn close(&mut self, id: WindowId) -> bool {
        let Some(index) = self
            .windows
            .iter()
            .position(|entry| entry.handle.window_id() == id)
        else {
            return false;
        };

        let entry = self.windows.remove(index);

        if self.windows.is_empty() {
            self.last_closed = Some(entry.state);
        }

        true
    }

    pub(crate) fn take_last_closed(&mut self) -> Option<WindowLocalState> {
        self.last_closed.take()
    }
}

/// The window that most recently gained focus; CLI `new_tab`/`activate`
/// target it. `None` until any window activates.
pub(crate) struct LastActiveWindow(pub(crate) Option<WindowId>);

impl Global for LastActiveWindow {}

pub(crate) fn selected_window_appearance(cx: &App) -> WindowAppearance {
    if ComponentTheme::global(cx).is_dark() {
        WindowAppearance::Dark
    } else {
        WindowAppearance::Light
    }
}

/// The workspace sidebar's width until the user drags it. The default is
/// raised to the minimum where the window buttons need more leading room.
const DEFAULT_SIDEBAR_WIDTH: f32 = f32::max(
    workspace_sidebar::SIDEBAR_WIDTH,
    workspace_sidebar::MIN_WIDTH,
);

/// The settings content lays each row out as label beside control only while
/// its panel is wider than 480; at 480 or narrower every row stacks and the
/// page reflows. Two points above that threshold keep rounding at fractional
/// scale factors from tipping it over.
const SETTINGS_CONTENT_MIN_WIDTH: f32 = 482.0;

/// Narrower than this, with the workspace sidebar at its default width, the
/// settings page stacks its rows and the agent composer wraps its toolbar
/// onto a second line; the tightest of the two is settings, whose navigation
/// pane has a fixed width. The sidebar default differs per platform (216 on
/// macOS, 180 on Windows), so the minimum does too: 938 and 902. Shorter
/// than the minimum height a terminal pane stops showing a usable number of
/// rows. The platform enforces both during interactive resize, not just for
/// the initial geometry.
pub(super) const MIN_WINDOW_WIDTH: f32 =
    DEFAULT_SIDEBAR_WIDTH + SETTINGS_NAV_WIDTH_PX + SETTINGS_CONTENT_MIN_WIDTH;

const MIN_WINDOW_HEIGHT: f32 = 400.0;

/// The identifier a display is saved under. The uuid is derived from the
/// device name, so it survives a restart; the platform display id is a
/// monitor handle that does not.
fn display_key(display: &dyn PlatformDisplay) -> Option<String> {
    display.uuid().ok().map(|uuid| uuid.to_string())
}

/// Open a terminal window using the saved geometry and session. Register its
/// state before constructing the view so workspace updates during restoration
/// can publish the resulting session.
pub(crate) fn open_window(
    cx: &mut App,
    initial: WindowLocalState,
    initial_cwd: Option<String>,
) -> WindowHandle<Root> {
    // The saved display, when it is still connected. Without it gpui places
    // the window on the primary display and converts the saved logical pixels
    // with that display's scale factor, which is wrong for geometry saved on
    // a display scaled differently.
    let display_id = initial
        .window
        .as_ref()
        .and_then(|w| w.display.as_deref())
        .and_then(|saved| {
            cx.displays()
                .into_iter()
                .find(|display| display_key(display.as_ref()).as_deref() == Some(saved))
        })
        .map(|display| display.id());

    let window_bounds = match &initial.window {
        Some(w) => {
            // WM_GETMINMAXINFO bounds interactive resize only, so geometry
            // saved by an older build (or edited by hand) is clamped here
            // as well; otherwise the window would restore below its
            // minimum and stay there until the user resized it.
            let bounds = Bounds::new(
                point(px(w.x), px(w.y)),
                size(
                    px(w.width.max(MIN_WINDOW_WIDTH)),
                    px(w.height.max(MIN_WINDOW_HEIGHT)),
                ),
            );

            if w.maximized {
                WindowBounds::Maximized(bounds)
            } else {
                WindowBounds::Windowed(bounds)
            }
        }
        None => WindowBounds::Windowed(Bounds::centered(None, size(px(960.0), px(620.0)), cx)),
    };

    // Titlebar setup for a shell window. A host that keeps drawing its own
    // window buttons over the transparent titlebar centers them in a
    // standard-height strip; this bar is taller, so the buttons are re-anchored
    // to its middle to line up with the controls the bar draws itself. A host
    // that draws its controls as part of the bar needs no such adjustment.
    let titlebar_options = TitlebarOptions {
        title: Some(t!("app-window-title").into()),
        appears_transparent: true,
        traffic_light_position: Host::WINDOW_CONTROLS_INSET
            .map(|inset| point(px(inset), px(inset))),
    };

    cx.open_window(
        WindowOptions {
            window_bounds: Some(window_bounds),
            display_id,
            // Borderless: the app draws its own titlebar (gpui-component
            // `TitleBar`); the Windows backend routes controls/drag/resize.
            window_decorations: Some(WindowDecorations::Client),
            titlebar: Some(titlebar_options),
            window_background: ui::window_background_appearance(cx),
            window_appearance_override: Some(selected_window_appearance(cx)),
            window_min_size: Some(size(px(MIN_WINDOW_WIDTH), px(MIN_WINDOW_HEIGHT))),
            ..Default::default()
        },
        // Wrap the shell in gpui-component's `Root` so modal/dialog layers
        // render. The shell focuses its active pane on first render.
        move |window, cx| {
            let view: AnyView = cx
                .new(|cx| {
                    let view = cx.weak_entity();

                    cx.global_mut::<WindowRegistry>().register(
                        window.window_handle(),
                        view,
                        initial,
                    );

                    AppWindow::new(initial_cwd, window, cx)
                })
                .into();

            // Each top-level region paints the configured alpha once. A
            // background on Root would sit underneath all of them and make
            // the effective opacity higher than the requested value.
            cx.new(|cx| Root::new(view, window, cx).bg(transparent_black()))
        },
    )
    .expect("open GPUI terminal window")
}

/// A workspace cwd as a shell working directory: `None` for empty or the
/// legacy `"."` placeholder (shells then start in their default directory).
pub(super) fn explicit_cwd(cwd: &str) -> Option<String> {
    let cwd = cwd.trim();

    (!cwd.is_empty() && cwd != ".").then(|| cwd.to_string())
}

/// The directory list an Agent Tab of `roots` starts with. Placeholder entries
/// are dropped for the same reason [`explicit_cwd`] drops them: they name no
/// directory a harness could be pointed at.
/// What a pane's control sheet closes its tab with: the close shortcut's
/// action, so the sheet closes the tab the same way the keyboard would.
fn close_tab() -> SheetClose {
    Arc::new(|window, cx| window.dispatch_action(Box::new(CloseTab), cx))
}

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

/// A conversation to reopen in a tab rooted where it ran, with the profile
/// that continues it: the listing tab's own, or the one of the agent that
/// recorded it. A conversation that names no directory opens with `fallback`,
/// the listing tab's directories.
pub(super) struct PendingAgentResume {
    pub(super) profile: AgentProfile,
    pub(super) cwd: Option<String>,
    pub(super) fallback: AgentWorkspace,
    pub(super) summary: SessionSummary,
}

/// A blank agent tab to relaunch in place on `profile`, optionally going on to
/// continue the conversation `resume` lists once the new session is ready.
pub(super) struct PendingAgentSwitch {
    pub(super) tab: TabId,
    pub(super) profile: AgentProfile,
    pub(super) resume: Option<SessionSummary>,
}

/// The launch profile last picked in a composer's agent control, by name.
/// The unified New Agent Tab entry opens it, so a user who works with one
/// agent picks it once instead of on every new tab. It lasts for the process
/// only: the configured default profile is what a fresh start opens.
#[derive(Default)]
pub(crate) struct PickedAgentProfile(Option<String>);

impl Global for PickedAgentProfile {}

/// The profile a unified New Agent Tab opens: the one last picked in a
/// composer while it still exists, otherwise the configured default.
pub(crate) fn unified_agent_profile(cx: &App) -> AgentProfile {
    let settings = cx.global::<AppSettings>();

    cx.try_global::<PickedAgentProfile>()
        .and_then(|picked| picked.0.as_deref())
        .and_then(|name| {
            settings
                .config()
                .agent_profiles
                .list
                .iter()
                .find(|profile| profile.name == name)
                .cloned()
        })
        .unwrap_or_else(|| settings.default_agent_profile_entry())
}

/// A tab's fallback title for an agent profile: the profile's name, so two
/// profiles of one agent stay distinguishable, or the agent's own name for an
/// unnamed profile.
fn agent_tab_title(profile: &AgentProfile) -> String {
    if profile.name.trim().is_empty() {
        profile.kind.display().to_string()
    } else {
        profile.name.clone()
    }
}

/// What a window did with a paired device's request to close a session.
pub(crate) enum DeviceClose {
    Closed,
    /// No tab of this window shows the session.
    NotHere,
    /// The session's tab is the last of the window's last workspace, which
    /// only the person at the host may close.
    LastWorkspace,
}

pub(crate) struct AppWindow {
    pub(crate) workspaces: WorkspaceManager,

    /// The remote workspace holding the tabs that follow each paired host's
    /// sessions, by host id.
    remote_workspaces: collections::HashMap<String, WorkspaceId>,

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
    /// next launch. Only set on the quit path; cancelling keeps everything.
    pub(crate) doomed_workspace: Option<WorkspaceId>,

    settings_close_pending: bool,
}

impl Drop for AppWindow {
    fn drop(&mut self) {
        remove_native_notifications(&self.agent_notifications.agent_monitor.notifications());
    }
}

impl AppWindow {
    pub(crate) fn new(
        initial_cwd: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe_global_in::<AppSettings>(window, |this, window, cx| {
            this.sync_team_setting(window, cx);
            this.sync_orchestration_setting(window, cx);

            cx.notify();
        })
        .detach();

        cx.observe_global::<AgentUpdates>(|_, cx| cx.notify())
            .detach();

        // Tabs mark the paired devices viewing them, and the title bar counts
        // the connected ones.
        cx.observe_global::<Remote>(|_, cx| cx.notify()).detach();

        // Stash the window geometry on every move/resize; main.rs flushes it
        // to local_state.toml on quit. Fires for both, and the Maximized
        // variant includes the restore bounds. Scan-and-update only: a stale
        // event after the window's entry is removed is a no-op.
        let window_id = window.window_handle().window_id();

        cx.observe_window_bounds(window, Self::on_window_bounds_changed)
            .detach();

        // OS-level close requests (Alt+F4, taskbar, system menu) go through
        // the running-processes confirmation. The titlebar X bypasses
        // WM_CLOSE, so it routes through the same check via `on_close_window`.
        let weak = cx.weak_entity();

        window.on_window_should_close(cx, move |window, cx| {
            // The shell closes the window itself once settings are saved; a
            // shell that is already gone has nothing left to save.
            weak.update(cx, |this, cx| this.request_window_close(window, cx))
                .is_err()
        });

        cx.observe_window_activation(window, Self::on_window_activation)
            .detach();

        // A running tab's pane withdraws its session from paired devices as
        // it is released, but a tab still waiting to be started has no pane,
        // so the window withdraws those as it goes; a closed window's tabs
        // would otherwise stay listed with nothing left to start.
        cx.on_release(|this, cx| {
            let pending: Vec<String> = this
                .workspaces
                .all_tabs()
                .flat_map(|tabs| tabs.list().items())
                .filter_map(|tab| tab.surface().pending_session().map(str::to_owned))
                .collect();

            for id in pending {
                ui::remote::withdraw_pending_session(&id, cx);
            }
        })
        .detach();

        let default_profile = cx.global::<AppSettings>().default_profile_command();
        let registry_entry = cx.global::<WindowRegistry>().get(window_id);

        // A CLI new_window target replaces session restore for this window.
        let remembered_session = if initial_cwd.is_some() {
            None
        } else {
            registry_entry.and_then(|entry| entry.session.clone())
        };

        let sidebar_width = registry_entry
            .and_then(|entry| entry.sidebar_width)
            .map(|width| width.clamp(workspace_sidebar::MIN_WIDTH, workspace_sidebar::MAX_WIDTH))
            .unwrap_or(DEFAULT_SIDEBAR_WIDTH);

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

                for route in tab.surface().agent_routes(cx) {
                    agent_monitor.register_route(route, activity_policy, now);
                }
            }
        }

        let git_model = cx.new(GitStatusModel::new);

        let mut this = Self {
            workspaces,
            remote_workspaces: collections::HashMap::new(),
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
                let tasks = cx.new(|_| BackgroundTasksView::new());
                let workflows = cx.new(|_| WorkflowsView::new());
                let panel = cx.new(|_| RightPanel::new(tasks, workflows));

                RightPanelController::new(panel, git_model)
            },
            update_notifications: UpdateNotificationLayer::default(),
            root_availability: RootAvailability::default(),
            doomed_workspace: None,
            settings_close_pending: false,
        };

        this.refresh_root_availability(cx);

        this
    }

    fn on_window_bounds_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = window.window_handle().window_id();
        let window_bounds = window.window_bounds();
        let bounds = window_bounds.get_bounds();

        let display = window
            .display(cx)
            .and_then(|display| display_key(display.as_ref()));

        if let Some(entry) = cx.global_mut::<WindowRegistry>().get_mut(id) {
            entry.window = Some(WindowState {
                x: bounds.origin.x.as_f32(),
                y: bounds.origin.y.as_f32(),
                width: bounds.size.width.as_f32(),
                height: bounds.size.height.as_f32(),
                maximized: matches!(window_bounds, WindowBounds::Maximized(_)),
                display,
            });
        }
    }

    fn on_window_activation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.window_active = Self::exact_window_active(window);

        if self.window_active {
            cx.global_mut::<LastActiveWindow>().0 = Some(self.window_id);

            // Coming back to a window is the person being back at this
            // computer, which ends pushes to devices they took away.
            remote::note_local_use(cx);

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
    /// terminal (an agent tab). Terminal-only entry points that also run while an
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
    /// panes' agent routes; the startup registration sweep only saw tabs that
    /// were live at window creation.
    fn ensure_active_tab_live(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(
            self.workspaces.active_tabs().active(),
            TabSurface::Hibernated(_)
        ) {
            return;
        }

        self.start_active_tab(window, cx);
    }

    fn start_active_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !materialize_active_tab(&mut self.workspaces, &mut self.next_id, window, cx) {
            return;
        }

        let surface = self.workspaces.active_tabs().active();

        let activity_policy = match surface {
            TabSurface::Agent(_) => AgentActivityPolicy::ExplicitLifecycle,
            _ => AgentActivityPolicy::ExpireAfterInactivity,
        };

        let routes = surface.agent_routes(cx);

        let now = time::Instant::now();

        for route in routes {
            self.agent_notifications
                .agent_monitor
                .register_route(route, activity_policy, now);
        }
    }

    fn wake_tab(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        if self.workspaces.active_tabs().list().active_id() != id
            || !matches!(
                self.workspaces.active_tabs().active(),
                TabSurface::Hibernated(_)
            )
        {
            return;
        }

        self.start_active_tab(window, cx);
        self.show_active_tab(window, cx);
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

    fn sync_orchestration_setting(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if cx
            .global::<AppSettings>()
            .config()
            .agent
            .enable_agent_orchestration
        {
            if matches!(
                self.workspaces.active_tabs().active(),
                TabSurface::OrchestrationDisabled(_)
            ) {
                self.on_active_tab_changed(window, cx);
            }

            return;
        }

        let tabs: Vec<_> = self
            .workspaces
            .all_tabs()
            .flat_map(|tabs| tabs.list().items())
            .filter(|tab| tab.surface().orchestration().is_some())
            .map(Tab::id)
            .collect();

        for id in tabs {
            if let Some(tab) = self
                .workspaces
                .tabs_for_tab_mut(id)
                .and_then(|tabs| tabs.list_mut().find_mut(id))
            {
                tab.surface_mut().disable_orchestration(cx);
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
        self.sync_git_tab_visibility(cx);

        let tabs = self.workspaces.active_tabs_mut();

        if tabs.clear_active_bell() | tabs.clear_active_outcome() {
            cx.notify();
        }

        self.acknowledge_visible(window, true, cx);
    }

    /// Put the active workspace, tab, and pane on screen after any of them
    /// changed: refresh the tab's own state, move keyboard focus into it,
    /// record the session a restore would rebuild, and repaint.
    pub(super) fn show_active_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.on_active_tab_changed(window, cx);

        self.focus_active(window, cx);

        cx.notify();
    }

    /// Switch to the workspace at `index`. The workspace keeps its own active
    /// tab, so the user returns to the tab they last used there.
    pub(super) fn activate_workspace(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.workspaces.list_mut().activate(index);

        self.show_active_tab(window, cx);
    }

    pub(crate) fn focus_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(
            self.workspaces.active_tabs().active(),
            TabSurface::Hibernated(_)
        ) {
            window.focus(&self.focus, cx);

            return;
        }

        if let Some(git) = self.workspaces.active_tabs().active().git() {
            let view = git.view.clone();

            view.update(cx, |view, cx| view.focus(window, cx));

            return;
        }

        // Settings owns its controls' focus and has no pane to focus.
        if self.workspaces.active_tabs().active().is_settings() {
            window.focus(&self.focus, cx);

            return;
        }

        if let Some(team) = self.workspaces.active_tabs().active().team().cloned() {
            team.update(cx, |pane, cx| pane.focus(window, cx));

            return;
        }

        if let Some(pane) = self
            .workspaces
            .active_tabs()
            .active()
            .orchestration()
            .cloned()
        {
            pane.update(cx, |pane, cx| pane.focus(window, cx));

            return;
        }

        if matches!(
            self.workspaces.active_tabs().active(),
            TabSurface::TeamUnavailable { .. }
                | TabSurface::TeamDisabled(_)
                | TabSurface::OrchestrationDisabled(_)
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

        self.agent_notifications.acknowledge(&route, &id, cx);
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
            .enumerate()
            // Remote entries are listed under their host, after this
            // computer's workspaces.
            .filter(|(_, summary)| summary.kind != WorkspaceKind::Remote)
            .map(|(index, summary)| {
                let tabs = self.workspaces.tabs_of(summary.id);

                let routes: Vec<_> = tabs
                    .into_iter()
                    .flat_map(|tabs| tabs.list().items())
                    .flat_map(|tab| tab.surface().agent_routes(cx))
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
                    index,
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
    /// OSC 133 instead of by an agent route.
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
            let routes = tab.surface().agent_routes(cx);
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
            default_shell_name()
        } else {
            tab.title()
        };

        if tab.exited() {
            t!("shell-tab-exited-title", title = base).into_owned()
        } else {
            base.to_string()
        }
    }

    pub(super) fn on_toggle_sidebar(
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
        let tab = self.workspaces.active_tabs().list().active_id();

        let id = self
            .workspaces
            .active_tabs()
            .active()
            .live()
            .tree()
            .focused();

        let pane = self.active_pane();
        let settings = cx.global::<AppSettings>();

        let count = if settings.config().system.warn_before_terminating_shell
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
            self.close_pane_now(tab, id, window, cx);

            return;
        }

        let description = close_description(
            count,
            "shell-close-pane-description",
            "shell-close-pane-processes-description",
        );

        open_close_confirm(
            window,
            cx,
            t!("shell-close-pane-title"),
            description,
            None,
            move |this, window, cx| this.close_pane_now(tab, id, window, cx),
        );
    }

    /// Close pane `id` of a split tab. The tab need not be active: a paired
    /// device may close a pane of any tab.
    fn close_pane_now(
        &mut self,
        tab: TabId,
        id: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let was_active = self.workspaces.active_tabs().list().active_id() == tab;

        let Some(tree) = self
            .workspaces
            .tabs_for_tab_mut(tab)
            .and_then(|tabs| tabs.list_mut().find_mut(tab))
            .and_then(|tab| tab.surface_mut().tree_mut())
        else {
            return;
        };

        let Some(pane) = tree.remove(id, cx) else {
            return;
        };

        let route = pane.read(cx).agent_route().clone();

        self.agent_notifications.remove_route(&route, cx);

        pane.read(cx).end_remote_session();

        // Dropping the pane entity drops its surface, releasing the IO thread
        // and ConPTY handle (same Drop chain as a tab close).
        drop(pane);

        // Refocusing a tab the user is not looking at would pull keyboard
        // focus away from what they are doing.
        if was_active {
            self.show_active_tab(window, cx);
        } else {
            cx.notify();
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

    /// Child processes running across every pane of this tab, summed over
    /// each shell's Job Object. The count enriches warnings but is not needed
    /// by the `Always` mode.
    fn close_process_count(&self, tree: &TabSurface, cx: &App) -> io::Result<usize> {
        let settings = cx.global::<AppSettings>();

        if settings.config().system.warn_before_terminating_shell
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
    /// tab, not on the active one.
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
        let unsaved = unsaved_orchestrations([surface], cx);

        if !unsaved.is_empty() {
            ask_about_unsaved_orchestrations(
                unsaved,
                move |this, window, cx| this.request_close_tab(id, window, cx),
                window,
                cx,
            );

            return;
        }

        let is_settings = surface.is_settings();

        // Closing a tab that follows a paired host's agent ends only this
        // view; the conversation runs on there, so there is nothing to warn
        // about.
        let is_agent = surface.is_agent()
            && !surface
                .agent()
                .is_some_and(|pane| pane.read(cx).remote_address().is_some());

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

        // A remote entry only gathers a host's tabs, so its last tab takes
        // it along without the warning that guards a local workspace's tabs.
        if last_tab && self.workspaces.kind_of(ws_id) == Some(WorkspaceKind::Remote) {
            self.close_workspace_now(ws_id, window, cx);

            return;
        }

        if last_tab {
            if self.workspaces.real_len() == 1 {
                self.confirm_close_last_workspace(ws_id, window, cx);

                return;
            }

            let description = close_description(
                count,
                if is_agent {
                    "shell-close-last-tab-agent-description"
                } else {
                    "shell-close-last-tab-description"
                },
                "shell-close-last-tab-processes-description",
            );

            open_close_confirm(
                window,
                cx,
                t!("shell-close-last-tab-title"),
                description,
                None,
                move |this, window, cx| this.close_workspace_now(ws_id, window, cx),
            );

            return;
        }

        if matches!(surface, TabSurface::Hibernated(_)) {
            self.close_tab_now(id, window, cx);

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
            close_description(
                count,
                "shell-close-tab-description",
                "shell-close-tab-processes-description",
            )
        };

        open_close_confirm(
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

        for route in tree.agent_routes(cx) {
            self.agent_notifications.remove_route(&route, cx);
        }

        for (_, pane) in tree.leaves() {
            pane.read(cx).end_remote_session();
        }

        if let Some(shared) = tree.pending_session() {
            ui::remote::withdraw_pending_session(shared, cx);
        }

        let return_to = tree.git().and_then(|git| git.return_to);

        drop(tree);

        if was_active {
            if let Some(index) = return_to.and_then(|id| {
                self.workspaces
                    .active_tabs()
                    .list()
                    .items()
                    .iter()
                    .position(|tab| tab.id() == id)
            }) {
                self.workspaces.active_tabs_mut().list_mut().activate(index);
            }

            self.on_active_tab_changed(window, cx);
        }

        self.focus_active(window, cx);

        cx.notify();
    }

    /// Whether the tab `id` can hibernate: it runs work a snapshot can
    /// restart, with another tab available to select when it is on screen.
    pub(crate) fn can_hibernate_tab(&self, id: TabId, cx: &App) -> bool {
        let Some(tabs) = self
            .workspaces
            .workspace_of_tab(id)
            .and_then(|workspace| self.workspaces.tabs_of(workspace))
        else {
            return false;
        };

        let lone_on_screen =
            tabs.list().len() == 1 && self.workspaces.active_tabs().list().active_id() == id;

        !lone_on_screen
            && tabs
                .list()
                .find(id)
                .is_some_and(|tab| tab.surface().can_hibernate(cx))
    }

    /// Ask before hibernating the tab `id`: its shells or agent process end,
    /// which loses whatever they were running, so this always confirms.
    pub(crate) fn request_hibernate_tab(
        &mut self,
        id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.can_hibernate_tab(id, cx) {
            return;
        }

        let Some(surface) = self
            .workspaces
            .workspace_of_tab(id)
            .and_then(|workspace| self.workspaces.tabs_of(workspace))
            .and_then(|tabs| tabs.list().find(id))
            .map(Tab::surface)
        else {
            return;
        };

        let description = if surface.is_agent() {
            t!("shell-hibernate-tab-agent-description").into_owned()
        } else {
            close_description(
                self.close_process_count(surface, cx),
                "shell-hibernate-tab-description",
                "shell-hibernate-tab-processes-description",
            )
        };

        open_close_confirm(
            window,
            cx,
            t!("shell-hibernate-tab-title"),
            description,
            None,
            move |this, window, cx| this.hibernate_tab_now(id, window, cx),
        );
    }

    /// Save the tab `id` and end its processes until the user wakes it.
    /// Select a neighbour first so hibernation does not interrupt navigation.
    fn hibernate_tab_now(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        // The tab may have changed while the dialog was open.
        if !self.can_hibernate_tab(id, cx) {
            return;
        }

        let list = self.workspaces.active_tabs().list();
        let was_active = list.active_id() == id;

        if was_active {
            let index = list.active_index();

            let neighbour = if index > 0 { index - 1 } else { index + 1 };

            self.workspaces
                .active_tabs_mut()
                .list_mut()
                .activate(neighbour);
        }

        let Some(surface) = hibernate_tab(&mut self.workspaces, id, window, cx) else {
            return;
        };

        for route in surface.agent_routes(cx) {
            self.agent_notifications.remove_route(&route, cx);
        }

        // Dropping the surface ends its shells and agent session.
        drop(surface);

        if was_active {
            self.show_active_tab(window, cx);
        }

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

        let unsaved = unsaved_orchestrations(
            self.workspaces
                .tabs_of(id)
                .into_iter()
                .flat_map(|tabs| tabs.list().items())
                .map(|tab| tab.surface()),
            cx,
        );

        if !unsaved.is_empty() {
            ask_about_unsaved_orchestrations(
                unsaved,
                move |this, window, cx| this.request_close_workspace(id, window, cx),
                window,
                cx,
            );

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

        let description = close_description(
            count,
            "shell-close-workspace-description",
            "shell-close-workspace-processes-description",
        );

        open_close_confirm(
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

        let description = close_description(
            process_count,
            "shell-close-temporary-workspaces-description",
            "shell-close-temporary-workspaces-processes-description",
        );

        open_close_confirm(
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
            // This command includes every temporary workspace by name, so a
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

        let message = close_description(
            count,
            "shell-close-last-workspace-message",
            "shell-close-last-workspace-processes-message",
        );

        // Quitting from here saves the session, so the same warning the
        // window-close dialog shows applies to this choice too.
        let note = self.temporary_workspace_note();

        let shell = cx.entity();

        window.open_dialog(cx, move |dialog, _, _| {
            close_last_workspace_dialog(dialog.centered(true), &shell, id, &message, &note)
        });
    }

    /// Exclude `id` from session persistence, so neither the quit hook nor a
    /// window close saves it into local_state.
    fn doom_workspace(&mut self, id: WorkspaceId) {
        self.doomed_workspace = Some(id);
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

    /// Save settings, then close the window unless the explicit confirmation
    /// setting or a terminal child-process warning holds it. Reached from the
    /// titlebar X and the OS close request (Alt+F4, taskbar); a close already
    /// waiting on its save is left to finish.
    pub(crate) fn request_window_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings_close_pending {
            return;
        }

        let unsaved = unsaved_orchestrations(
            self.workspaces
                .all_tabs()
                .flat_map(|tabs| tabs.list().items())
                .map(|tab| tab.surface()),
            cx,
        );

        if !unsaved.is_empty() {
            ask_about_unsaved_orchestrations(
                unsaved,
                |this, window, cx| this.request_window_close(window, cx),
                window,
                cx,
            );

            return;
        }

        self.settings_close_pending = true;

        let saved = ui::settings::save_settings(window, cx);

        cx.spawn_in(window, async move |this, cx| {
            let saved = saved.await;

            let _ = this.update_in(cx, |this, window, cx| {
                this.settings_close_pending = false;

                this.finish_window_close(saved, window, cx);
            });
        })
        .detach();
    }

    fn finish_window_close(&mut self, saved: bool, window: &mut Window, cx: &mut Context<Self>) {
        // Every way this window is removed passes here; the state kept for
        // reopening it must be current at that moment.
        self.sync_session_memory(cx);

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
            window.remove_window();

            return;
        }

        let mut description = close_description(
            count,
            "shell-close-window-description",
            "shell-close-window-processes-description",
        );

        if !saved {
            description.push_str("\n\n");

            description.push_str(&t!("settings-save-failed-close-description"));
        }

        let note = self.temporary_workspace_note();

        if !saved {
            open_save_failed_close(description, note, window, cx);

            return;
        }

        open_close_confirm(
            window,
            cx,
            t!("shell-close-window-title"),
            description,
            note,
            |_, window, _| window.remove_window(),
        );
    }

    fn close_workspace_now(
        &mut self,
        id: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.workspaces.kind_of(id) == Some(WorkspaceKind::Settings) {
            let saved = ui::settings::save_settings(window, cx);

            cx.spawn_in(window, async move |this, cx| {
                if saved.await {
                    let _ = this
                        .update_in(cx, |this, window, cx| this.remove_workspace(id, window, cx));
                }
            })
            .detach();
        } else {
            self.remove_workspace(id, window, cx);
        }
    }

    fn remove_workspace(&mut self, id: WorkspaceId, window: &mut Window, cx: &mut Context<Self>) {
        let routes = self
            .workspaces
            .tabs_of(id)
            .map(|tabs| {
                tabs.list()
                    .items()
                    .iter()
                    .flat_map(|tab| tab.surface().agent_routes(cx))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let restoring: Vec<String> = self
            .workspaces
            .tabs_of(id)
            .into_iter()
            .flat_map(|tabs| tabs.list().items())
            .filter_map(|tab| tab.surface().pending_session().map(str::to_owned))
            .collect();

        // A host terminal opened from a tab here ends with that tab, which
        // closing its workspace must not skip.
        let panes: Vec<_> = self
            .workspaces
            .tabs_of(id)
            .into_iter()
            .flat_map(|tabs| tabs.list().items())
            .flat_map(|tab| tab.surface().leaves())
            .map(|(_, pane)| pane.clone())
            .collect();

        let kind = self.workspaces.kind_of(id);

        let was_active = self.workspaces.list().active_id() == id;

        if self.workspaces.close_workspace(id).is_some() {
            for route in routes {
                self.agent_notifications.remove_route(&route, cx);
            }

            for shared in restoring {
                ui::remote::withdraw_pending_session(&shared, cx);
            }

            for pane in panes {
                pane.read(cx).end_remote_session();
            }

            match kind {
                Some(WorkspaceKind::Settings) => self.retire_settings_workspace(),
                Some(WorkspaceKind::Remote) => {
                    self.remote_workspaces
                        .retain(|_, workspace| *workspace != id);
                }
                Some(WorkspaceKind::Normal) | None => {}
            }

            if was_active {
                self.on_active_tab_changed(window, cx);
            }

            self.focus_active(window, cx);

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

        self.show_active_tab(window, cx);
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

        self.show_active_tab(window, cx);
    }

    /// Apply saved split ratios once their groups have real bounds (the first
    /// visible frames after a session restore); cleared after applying.
    pub(super) fn apply_pending_ratios(&mut self, cx: &mut Context<Self>) {
        let Some(tree) = self.workspaces.active_tabs_mut().active_mut().tree_mut() else {
            return;
        };

        tree.apply_pending_ratios(cx);
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

        self.show_active_tab(window, cx);
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

        self.leave_pseudo_workspace();

        let directory = config_dir_path();

        let runtime = match saved {
            Some(id) => TeamRuntime::open(&directory, id, cx),
            None => TeamRuntime::create(
                &directory,
                agent_workspace(self.workspaces.active_roots()),
                cx,
            ),
        };

        let surface = TabSurface::Team(cx.new(|cx| TeamPane::new(runtime, window, cx)));

        let id = Self::alloc_id(&mut self.next_id);

        self.insert_tab(
            TabId(id),
            surface,
            t!("team-title").into_owned(),
            window,
            cx,
        );
    }

    pub(crate) fn open_orchestration_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !cx
            .global::<AppSettings>()
            .config()
            .agent
            .enable_agent_orchestration
        {
            return;
        }

        self.leave_pseudo_workspace();

        let workspace = agent_workspace(self.workspaces.active_roots());

        let surface = TabSurface::Orchestration(
            cx.new(|cx| OrchestrationPane::new(config_dir_path(), workspace, None, window, cx)),
        );

        let id = Self::alloc_id(&mut self.next_id);

        self.insert_tab(
            TabId(id),
            surface,
            t!("orchestration-title").into_owned(),
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

        let display = window
            .display(cx)
            .and_then(|display| display_key(display.as_ref()));

        open_window(
            cx,
            WindowLocalState {
                window: Some(WindowState {
                    x: bounds.origin.x.as_f32() + 30.0,
                    y: bounds.origin.y.as_f32() + 30.0,
                    width: bounds.size.width.as_f32(),
                    height: bounds.size.height.as_f32(),
                    maximized: false,
                    display,
                }),
                session: None,
                // New windows inherit this window's sidebar width.
                sidebar_width: Some(self.sidebar.width),
            },
            None,
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
        // `leave_pseudo_workspace` can change which workspace is active, so
        // the primary directory is read only after the move.
        self.leave_pseudo_workspace();

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
        self.leave_pseudo_workspace();

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

    /// Show `session` on the paired host `host`: the tab already following
    /// it if there is one, otherwise a new tab following it.
    pub(crate) fn open_remote_session(
        &mut self,
        host: &DeviceId,
        session: &SessionInfo,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let found = self
            .workspaces
            .all_tabs()
            .enumerate()
            .find_map(|(workspace_index, tabs)| {
                tabs.list()
                    .items()
                    .iter()
                    .position(|tab| {
                        tab.surface()
                            .follows_remote(host.as_str(), &session.session, cx)
                    })
                    .map(|tab_index| (workspace_index, tab_index))
            });

        match found {
            Some((workspace_index, tab_index)) => {
                self.jump_to_tab(workspace_index, tab_index, window, cx)
            }
            // Opening one inserts a tab into this window, which is being
            // updated now; it runs once this update is done.
            None => {
                let host = host.clone();
                let session = session.clone();

                window.defer(cx, move |window, cx| {
                    ui::remote::open_session(&host, &session, window, cx)
                });
            }
        }
    }

    /// Open a tab on a terminal a paired device started on this computer.
    pub(crate) fn open_local_view(
        &mut self,
        view: LocalView,
        title: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.leave_pseudo_workspace();

        let id = Self::alloc_id(&mut self.next_id);

        let pane = match spawn_local_view_pane(cx, id, view, title.clone()) {
            Ok(pane) => pane,
            Err(error) => {
                warn!("a view of a remote-created terminal failed to start: {error}");

                return;
            }
        };

        Self::watch_pane(&pane, cx);

        self.insert_tab(
            TabId(id),
            TabSurface::Live(TerminalLayout::new_leaf(PaneId(id), pane)),
            title,
            window,
            cx,
        );
    }

    /// Open a tab on a terminal that runs on another computer. With
    /// `ends_with_tab`, closing the tab ends the session on the host.
    pub(crate) fn open_remote_terminal(
        &mut self,
        pty: NetworkPty,
        ends_with_tab: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = Self::alloc_id(&mut self.next_id);
        let host = pty.host().id().as_str().to_owned();
        let host_name = pty.host().name();

        let pane = match spawn_remote_pane(cx, id, pty, ends_with_tab) {
            Ok(pane) => pane,
            Err(error) => {
                warn!("remote terminal failed to start: {error}");

                return;
            }
        };

        Self::watch_pane(&pane, cx);

        self.insert_remote_tab(
            host,
            TabId(id),
            TabSurface::Live(TerminalLayout::new_leaf(PaneId(id), pane)),
            host_name,
            window,
            cx,
        );
    }

    /// Open a tab following an agent session on a paired host. The session
    /// keeps running there when the tab closes.
    pub(crate) fn open_remote_agent_tab(
        &mut self,
        owner: SessionOwner,
        pane: Entity<AgentPane>,
        title: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((host, _)) = pane.read(cx).remote_address() else {
            return;
        };

        let id = Self::alloc_id(&mut self.next_id);

        Self::watch_agent_tab(&pane, None, cx);

        self.insert_remote_tab(
            host,
            TabId(id),
            TabSurface::Agent(AgentTab { owner, pane }),
            title,
            window,
            cx,
        );
    }

    /// Add a tab following one of `host`'s sessions to that host's remote
    /// workspace, creating the workspace on the host's first tab, and show
    /// it. The session runs on the host, so the tab never joins a local
    /// workspace, where it would pass for a tab running on this computer.
    fn insert_remote_tab(
        &mut self,
        host: String,
        id: TabId,
        surface: TabSurface,
        title: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let existing = self
            .remote_workspaces
            .get(&host)
            .copied()
            .and_then(|workspace| self.workspaces.list().index_of(workspace));

        match existing {
            Some(index) => {
                self.workspaces.list_mut().activate(index);

                self.insert_tab(id, surface, title, window, cx);
            }
            None => {
                let tabs = TabManager::new(surface, id, title);
                let workspace = WorkspaceId(Self::alloc_id(&mut self.next_id));
                let name = remote::paired_host_name(&host, cx).unwrap_or_else(|| host.clone());

                self.workspaces.new_workspace_of_kind(
                    tabs,
                    workspace,
                    name,
                    // The host's directories are not this computer's, so
                    // the entry owns none and never takes part in routing
                    // a local path to a workspace.
                    None,
                    WorkspaceKind::Remote,
                );

                self.remote_workspaces.insert(host, workspace);

                self.show_active_tab(window, cx);
            }
        }
    }

    /// The workspaces a paired device may start an agent in: this window's
    /// normal workspaces, by id, name and primary directory.
    pub(crate) fn device_workspaces(&self) -> Vec<WorkspaceInfo> {
        self.workspaces
            .summaries()
            .into_iter()
            .filter(|workspace| {
                workspace.kind == WorkspaceKind::Normal && !workspace.cwd.is_empty()
            })
            // Sessions name their workspace by this label, so a device can
            // set each one under the workspace offered with the same name.
            .map(|workspace| WorkspaceInfo {
                id: Some(remote::device_workspace_id(self.window_id, workspace.id)),
                name: workspace_display_label(&workspace.name, &workspace.cwd),
                path: workspace.cwd,
            })
            .collect()
    }

    /// Start an agent tab for a paired device in the workspace whose primary
    /// directory is `path`, without switching to it: the person at the host
    /// keeps what they are looking at. Returns the id devices know the tab
    /// by, or `None` when no workspace here has that directory.
    pub(crate) fn open_agent_tab_for_device(
        &mut self,
        profile: &AgentProfile,
        path: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<String> {
        let workspace_id = self
            .workspaces
            .summaries()
            .into_iter()
            .find(|workspace| workspace.kind == WorkspaceKind::Normal && workspace.cwd == path)?
            .id;

        let workspace = agent_workspace(self.workspaces.roots_of(workspace_id));
        let id = Self::alloc_id(&mut self.next_id);

        let title = if profile.name.trim().is_empty() {
            profile.kind.display().to_string()
        } else {
            profile.name.clone()
        };

        let owner = AgentSession::create(profile.clone(), workspace, None, cx);
        let pane = cx.new(|cx| AgentPane::attach(&owner, window, cx));

        Self::watch_agent_tab(&pane, None, cx);

        self.register_agent_tab(&pane, cx);

        owner.start(None, cx);

        let shared = ui::remote::shared_agent_id(&pane, cx);

        self.workspaces.tabs_of_mut(workspace_id)?.append_tab(
            TabSurface::Agent(AgentTab { owner, pane }),
            TabId(id),
            title,
        );

        // Render keeps session workspaces current, but a minimized or
        // occluded host window may not render for a long time, and until it
        // does the device would list the new session under no workspace.
        ui::remote::sync_workspaces(self.window_id, &self.workspaces, cx);

        cx.notify();

        shared
    }

    /// Start a terminal tab running `profile` for a paired device in the
    /// workspace whose primary directory is `path`, without switching to it:
    /// the person at the host keeps what they are looking at. Returns the id
    /// devices know the tab by; the error tells the device why none opened.
    pub(crate) fn open_terminal_tab_for_device(
        &mut self,
        profile: (Option<String>, Vec<String>),
        path: &str,
        cx: &mut Context<Self>,
    ) -> Result<String, String> {
        let workspace_id = self
            .workspaces
            .summaries()
            .into_iter()
            .find(|workspace| workspace.kind == WorkspaceKind::Normal && workspace.cwd == path)
            .ok_or_else(|| format!("{path} is not a workspace on this computer"))?
            .id;

        let cwd = self
            .workspaces
            .roots_of(workspace_id)
            .and_then(|roots| explicit_cwd(roots.primary()));

        let id = Self::alloc_id(&mut self.next_id);
        let pane = spawn_default_pane(cx, id, profile, cwd);

        self.register_agent_pane(&pane, cx);

        let title = pane.read(cx).profile_name().to_string();
        let shared = remote::shared_tab_id(&pane, cx);

        self.workspaces
            .tabs_of_mut(workspace_id)
            .ok_or("the workspace closed")?
            .append_tab(
                TabSurface::Live(TerminalLayout::new_leaf(PaneId(id), pane)),
                TabId(id),
                title,
            );

        // Render keeps session workspaces current, but a minimized or
        // occluded host window may not render for a long time, and until it
        // does the device would list the new session under no workspace.
        remote::sync_workspaces(self.window_id, &self.workspaces, cx);

        cx.notify();

        shared.ok_or_else(|| "the terminal was not shared".to_owned())
    }

    /// Close what shows a session a paired device asked to end: `pane`, the
    /// pane the session is shared from, or the still-pending agent tab
    /// devices know by `session`. The device already confirmed, so no dialog
    /// asks again. A split tab loses only that pane; a tab closing as the
    /// last of its workspace takes the workspace with it, as it does when
    /// its user closes it, but never the window's last workspace, which
    /// would end the window for the person at the host.
    pub(crate) fn close_for_device(
        &mut self,
        pane: Option<EntityId>,
        session: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> DeviceClose {
        let found = self
            .workspaces
            .all_tabs()
            .flat_map(|tabs| tabs.list().items())
            .find(|tab| {
                let surface = tab.surface();

                surface.pending_session() == Some(session)
                    || pane.is_some_and(|pane| surface.pane_ids().contains(&pane))
            })
            .map(|tab| {
                let surface = tab.surface();

                let leaf = surface
                    .tree()
                    .filter(|tree| !tree.tree().is_single_leaf())
                    .and_then(|_| {
                        surface
                            .leaves()
                            .into_iter()
                            .find(|(_, leaf)| Some(leaf.entity_id()) == pane)
                    })
                    .map(|(id, _)| id);

                (tab.id(), leaf)
            });

        let Some((tab, leaf)) = found else {
            return DeviceClose::NotHere;
        };

        if let Some(leaf) = leaf {
            self.close_pane_now(tab, leaf, window, cx);

            return DeviceClose::Closed;
        }

        let Some(workspace) = self.workspaces.workspace_of_tab(tab) else {
            return DeviceClose::NotHere;
        };

        let last_tab = self
            .workspaces
            .tabs_of(workspace)
            .is_some_and(|tabs| tabs.list().len() == 1);

        match (last_tab, self.workspaces.real_len()) {
            (false, _) => self.close_tab_now(tab, window, cx),
            (true, 1) => return DeviceClose::LastWorkspace,
            (true, _) => self.close_workspace_now(workspace, window, cx),
        }

        DeviceClose::Closed
    }

    /// Open an agent tab: an agent chat conversation in place of a terminal.
    /// The conversation's agent process starts in the workspace cwd, at once
    /// or, for a unified tab, with its first message.
    pub(crate) fn open_agent_tab(
        &mut self,
        profile: AgentProfile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.leave_pseudo_workspace();

        let workspace = agent_workspace(self.workspaces.active_roots());

        if !cx.global::<AppSettings>().config().agent.unified_agent_tab {
            self.open_agent_tab_in(&profile, workspace, None, window, cx);

            return;
        }

        // A unified tab may still change agent in its composer, so its
        // harness waits for the first message, like a tab relaunched on
        // another profile does. Starting it now would cover the blank tab
        // with the start layer for a process the next pick may retire.
        let id = Self::alloc_id(&mut self.next_id);
        let tab = self.create_agent_tab(&profile, workspace, None, window, cx);

        tab.pane.update(cx, |pane, cx| pane.defer_launch(cx));

        self.insert_tab(
            TabId(id),
            TabSurface::Agent(tab),
            agent_tab_title(&profile),
            window,
            cx,
        );
    }

    /// Open an agent tab rooted at `cwd`, optionally continuing the
    /// conversation `resume` lists once its session is ready. A conversation
    /// belongs to the directory it ran in, so one listed from another tab
    /// opens here, not in the tab that listed it.
    pub(super) fn open_agent_tab_in(
        &mut self,
        profile: &AgentProfile,
        workspace: AgentWorkspace,
        resume: Option<SessionSummary>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = Self::alloc_id(&mut self.next_id);
        let tab = self.launch_agent_tab(profile, workspace, resume, window, cx);

        self.insert_tab(
            TabId(id),
            TabSurface::Agent(tab),
            agent_tab_title(profile),
            window,
            cx,
        );
    }

    /// A started agent tab on `profile`, registered with this window's agent
    /// monitor and shared with paired devices, but not yet placed in any tab
    /// list.
    fn launch_agent_tab(
        &mut self,
        profile: &AgentProfile,
        workspace: AgentWorkspace,
        resume: Option<SessionSummary>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AgentTab {
        let tab = self.create_agent_tab(profile, workspace, resume, window, cx);

        tab.owner.start(None, cx);

        tab
    }

    /// `launch_agent_tab` without the launch, for a caller that decides
    /// when the harness starts.
    fn create_agent_tab(
        &mut self,
        profile: &AgentProfile,
        workspace: AgentWorkspace,
        resume: Option<SessionSummary>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AgentTab {
        let owner = AgentSession::create(profile.clone(), workspace, None, cx);
        let pane = cx.new(|cx| AgentPane::attach(&owner, window, cx));

        Self::watch_agent_tab(&pane, None, cx);

        self.register_agent_tab(&pane, cx);

        // The conversation is resumed through the history-list path once
        // the fresh session is ready: a start that includes the id replays
        // nothing for Claude Code and Codex and never names the tab.
        if let Some(summary) = resume {
            owner
                .session()
                .clone()
                .update(cx, |session, _| session.resume_when_ready(summary));
        }

        AgentTab { owner, pane }
    }

    /// Relaunch the agent tab `request.tab` on another profile, in the same
    /// place in its tab list and in a fresh conversation. The tab keeps its
    /// id and position and the unsent message moves across; the old session
    /// is dropped, which retires its agent process and ends any conversation
    /// it held.
    fn replace_agent_tab(
        &mut self,
        request: PendingAgentSwitch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let PendingAgentSwitch {
            tab: id,
            profile,
            resume,
        } = request;

        let Some(old_pane) = self
            .workspaces
            .all_tabs()
            .flat_map(|tabs| tabs.list().items())
            .find(|tab| tab.id() == id)
            .and_then(|tab| tab.surface().agent())
            .cloned()
        else {
            return;
        };

        let Some(workspace) = old_pane
            .read(cx)
            .agent_session()
            .map(|session| session.read(cx).workspace().clone())
        else {
            return;
        };

        // Picking an agent chooses which harness the next message launches,
        // not a request to launch it now, and flipping through agents would
        // otherwise start and retire one process per pick. Continuing a
        // listed conversation does need the harness.
        let deferred = resume.is_none();

        let draft = old_pane.update(cx, |pane, cx| pane.take_composer_draft(window, cx));
        let fresh = self.create_agent_tab(&profile, workspace, resume, window, cx);
        let pane = fresh.pane.clone();

        if deferred {
            pane.update(cx, |pane, cx| pane.defer_launch(cx));
        } else {
            fresh.owner.start(None, cx);
        }

        pane.update(cx, |pane, cx| {
            pane.restore_composer_draft(draft, window, cx)
        });

        let Some(tabs) = self.workspaces.tabs_for_tab_mut(id) else {
            return;
        };

        let old = tabs
            .list_mut()
            .find_mut(id)
            .map(|tab| mem::replace(tab.surface_mut(), TabSurface::Agent(fresh)));

        // A title the blank tab was given from its draft or history belongs
        // to the old agent's conversation, so the relaunched tab starts from
        // its new profile's name; a user-authored title still wins.
        tabs.set_title(id, String::new());
        tabs.set_default_title(id, agent_tab_title(&profile));

        if let Some(old) = old {
            for route in old.agent_routes(cx) {
                self.agent_notifications.remove_route(&route, cx);
            }
        }

        if self.workspaces.active_tabs().list().active_id() == id {
            self.focus_active(window, cx);
        }

        cx.notify();
    }

    pub(crate) fn on_new_agent_tab(
        &mut self,
        _: &NewAgentTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let profile = if cx.global::<AppSettings>().config().agent.unified_agent_tab {
            unified_agent_profile(cx)
        } else {
            cx.global::<AppSettings>().default_agent_profile_entry()
        };

        self.open_agent_tab(profile, window, cx);
    }

    /// Find a terminal by its current directory across this window's workspaces.
    /// A workspace root alone does not show where its shells are now.
    pub(crate) fn focus_dir_tab(
        &mut self,
        path: &path::Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let target = path_identity(path);

        let location =
            self.workspaces
                .all_tabs()
                .enumerate()
                .find_map(|(workspace_index, tabs)| {
                    tabs.list()
                        .items()
                        .iter()
                        .enumerate()
                        .find_map(|(tab_index, tab)| {
                            tab.surface()
                                .terminal_in_directory(&target, cx)
                                .map(|pane_index| (workspace_index, tab_index, pane_index))
                        })
                });

        let Some((workspace_index, tab_index, pane_index)) = location else {
            return false;
        };

        self.workspaces.list_mut().activate(workspace_index);

        self.workspaces
            .active_tabs_mut()
            .list_mut()
            .activate(tab_index);

        self.ensure_active_tab_live(window, cx);

        if let Some(tree) = self.workspaces.active_tabs_mut().active_mut().tree_mut()
            && let Some(pane_id) = tree.tree().leaves().get(pane_index).map(|(id, _)| *id)
        {
            tree.tree_mut().set_focused(pane_id);
        }

        window.activate_window();
        self.show_active_tab(window, cx);

        true
    }

    /// Open a terminal in the exact workspace, or a containing workspace when
    /// enabled. With no suitable workspace, create a temporary one at `path`.
    pub(crate) fn open_dir_tab(
        &mut self,
        path: &path::Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let summaries = self.workspaces.summaries();
        let target = path.display().to_string();

        let workspace = exact_match(&summaries, path).or_else(|| {
            cx.global::<AppSettings>()
                .config()
                .system
                .open_in_best_workspace
                .then(|| best_match(&summaries, path))
                .flatten()
        });

        let Some(ws_id) = workspace else {
            self.create_temporary_workspace(
                t!("shell-workspace-default-name").into(),
                WorkspaceRoots::single(target),
                window,
                cx,
            );

            return;
        };

        if let Some(index) = summaries.iter().position(|ws| ws.id == ws_id) {
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

        cx.notify();
    }

    /// Step the workspace's tab list on to its next fold.
    pub(crate) fn cycle_tab_fold(&mut self, id: WorkspaceId, cx: &mut Context<Self>) {
        self.workspaces.cycle_tab_fold(id);

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

        cx.notify();
    }

    pub(super) fn on_next_tab(&mut self, _: &NextTab, window: &mut Window, cx: &mut Context<Self>) {
        self.workspaces.active_tabs_mut().list_mut().focus_next();

        self.show_active_tab(window, cx);
    }

    pub(super) fn on_prev_tab(&mut self, _: &PrevTab, window: &mut Window, cx: &mut Context<Self>) {
        self.workspaces.active_tabs_mut().list_mut().focus_prev();

        self.show_active_tab(window, cx);
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
    /// Each jump shrinks the set instead of advancing a cursor of its own,
    /// because focusing a tab is what clears both marks.
    pub(super) fn next_ready_tab(&self, cx: &App) -> Option<(usize, usize)> {
        self.next_marked_tab(|tab| {
            let routes = tab.surface().agent_routes(cx);

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
    /// Unlike the ready set this one does not shrink when the tab is focused
    /// (watching a tab does not finish its work), so the jump keeps cycling
    /// while the same tabs stay busy, as following several parallel runs
    /// needs.
    pub(crate) fn next_busy_tab(&self, cx: &App) -> Option<(usize, usize)> {
        self.next_marked_tab(|tab| {
            if Self::tab_terminal_activity(tab, cx) == TerminalActivity::Running {
                return true;
            }

            let routes = tab.surface().agent_routes(cx);

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

        self.show_active_tab(window, cx);
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
        open_new_workspace_dialog(window, cx);
    }

    /// Adopt a temporary workspace: from here on it is saved with the session
    /// like any other. Already-persistent workspaces are unaffected.
    pub(crate) fn activate_as_workspace(&mut self, id: WorkspaceId, cx: &mut Context<Self>) {
        self.workspaces.set_temporary(id, false);

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

        self.activate_workspace(next, window, cx);
    }

    pub(super) fn on_prev_workspace(
        &mut self,
        _: &PrevWorkspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let len = self.workspaces.list().len();
        let prev = (self.workspaces.list().active_index() + len - 1) % len;

        self.activate_workspace(prev, window, cx);
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
        // Vertical tab rows list every workspace's tabs, and right-clicking a
        // row does not activate its workspace.
        let Some(current) = self
            .workspaces
            .tabs_for_tab(id)
            .and_then(|tabs| tabs.list().find(id))
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
                self.rename_tab(id, name, cx);
            }
        }

        self.focus_active(window, cx);

        cx.notify();
    }

    fn rename_tab(&mut self, id: TabId, name: String, cx: &mut Context<Self>) {
        let mut renamed_agent = None;

        if let Some(tabs) = self.workspaces.tabs_for_tab_mut(id) {
            tabs.rename(id, name.clone());

            renamed_agent = tabs
                .list()
                .find(id)
                .and_then(|tab| tab.surface().agent().cloned());
        }

        // An agent tab's name is the conversation's name, so it goes to the
        // harness too: its session listing is what this application's own
        // recent-sessions list reads.
        if let Some(agent) = renamed_agent {
            agent.update(cx, |agent, _| agent.rename_session(&name));
        }
    }

    /// Rename the tab showing a session a paired device renamed: the one
    /// holding `pane`, the pane the session is shared from, or the pending
    /// tab devices know by `session`. Returns whether this window holds it.
    pub(crate) fn rename_for_device(
        &mut self,
        pane: Option<EntityId>,
        session: &str,
        title: String,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(tab) = self.workspaces.find_tab_id(|surface| {
            surface.pending_session() == Some(session)
                || pane.is_some_and(|pane| surface.pane_ids().contains(&pane))
        }) else {
            return false;
        };

        self.rename_tab(tab, title, cx);

        // Devices hear the new name when the window next renders, which a
        // minimized or occluded window may not do for a long time.
        ui::remote::sync_workspaces(self.window_id, &self.workspaces, cx);

        cx.notify();

        true
    }

    /// Begin renaming `session` on the paired host `host` in its sidebar row.
    pub(crate) fn start_remote_rename(
        &mut self,
        host: DeviceId,
        session: SessionInfo,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.renames.begin_remote(host, session, window, cx);

        cx.notify();
    }

    /// End the in-flight rename of a paired host's session, with the same
    /// semantics as a tab rename. The host renames its tab, and the new name
    /// shows once it lists the session again.
    pub(crate) fn finish_remote_rename(
        &mut self,
        commit: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((host, session, input)) = self.renames.take_remote() else {
            return;
        };

        let name = input.read(cx).value().trim().to_string();

        if commit && !name.is_empty() && name != session.title {
            ui::remote::rename_session(&host, session.session, name, window, cx);
        }

        self.focus_active(window, cx);

        cx.notify();
    }

    /// Ask before closing the tabs here that follow `session` on `host`,
    /// leaving the session running on the host.
    pub(crate) fn request_disconnect_session(
        &mut self,
        host: DeviceId,
        host_name: String,
        session: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        open_close_confirm(
            window,
            cx,
            t!("remote-session-disconnect-title"),
            t!("remote-session-disconnect-description", host = &host_name).into_owned(),
            None,
            move |this, window, cx| this.close_following(&host, &session, window, cx),
        );
    }

    /// Ask before ending `session` on `host` for every device viewing it.
    /// Once the host ended it, the tabs here following it close too.
    pub(crate) fn request_close_remote_session(
        &mut self,
        host: DeviceId,
        host_name: String,
        session: SessionInfo,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        open_close_confirm(
            window,
            cx,
            t!("remote-session-close-title"),
            t!(
                "remote-session-close-description",
                title = &session.title,
                host = &host_name
            )
            .into_owned(),
            None,
            move |_, window, cx| {
                let shell = cx.entity();
                let closed_host = host.clone();
                let closed = session.session.clone();

                ui::remote::close_session(
                    &host,
                    session.session.clone(),
                    window,
                    cx,
                    move |window, cx| {
                        shell.update(cx, |this, cx| {
                            this.close_following(&closed_host, &closed, window, cx)
                        });
                    },
                );
            },
        );
    }

    /// Close what here follows `session` on `host`, leaving the session to
    /// the host: a split tab loses only the panes following it, and a remote
    /// entry's last tab takes the entry along, as closing it would.
    fn close_following(
        &mut self,
        host: &DeviceId,
        session: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tabs: Vec<TabId> = self
            .workspaces
            .all_tabs()
            .flat_map(|tabs| tabs.list().items())
            .filter(|tab| tab.surface().follows_remote(host.as_str(), session, cx))
            .map(|tab| tab.id())
            .collect();

        for tab in tabs {
            let Some(surface) = self
                .workspaces
                .tabs_for_tab(tab)
                .and_then(|tabs| tabs.list().find(tab))
                .map(|tab| tab.surface())
            else {
                continue;
            };

            let following: Vec<_> = surface
                .leaves()
                .into_iter()
                .filter(|(_, pane)| {
                    pane.read(cx).remote_tab().is_some_and(|remote| {
                        remote.host.id().as_str() == host.as_str() && remote.session == session
                    })
                })
                .map(|(leaf, pane)| (leaf, pane.clone()))
                .collect();

            // A terminal this computer started on the host ends with its
            // tab; closing what follows it here only detaches from it.
            for (_, pane) in &following {
                pane.update(cx, |pane, _| pane.keep_remote_session());
            }

            let split = surface
                .tree()
                .is_some_and(|tree| !tree.tree().is_single_leaf());

            let workspace = self.workspaces.workspace_of_tab(tab);

            let last_tab = workspace
                .and_then(|workspace| self.workspaces.tabs_of(workspace))
                .is_some_and(|tabs| tabs.list().len() == 1);

            match (split, workspace) {
                (true, _) if !following.is_empty() => {
                    for (leaf, _) in following {
                        self.close_pane_now(tab, leaf, window, cx);
                    }
                }
                (_, Some(workspace))
                    if last_tab
                        && self.workspaces.kind_of(workspace) == Some(WorkspaceKind::Remote) =>
                {
                    self.close_workspace_now(workspace, window, cx);
                }
                (_, Some(_)) if !last_tab => self.close_tab_now(tab, window, cx),
                // A local workspace's last tab stays, as it would after the
                // session ended on its own.
                _ => {}
            }
        }
    }

    /// Follow the active terminal's OSC7 directory or the active Agent's
    /// primary directory, falling back to the configured directory only when
    /// neither provides one. Rendering and CWD events both synchronize the
    /// target so tab switches and workspace directory edits refresh the summary.
    pub(super) fn sync_git_target(&self, cx: &mut Context<Self>) {
        let cwd = self
            .workspaces
            .active_tabs()
            .active()
            .git()
            .map(|tab| tab.view.read(cx).cwd().to_string())
            .or_else(|| {
                self.try_active_pane()
                    .and_then(|pane| pane.read(cx).tab_state().cwd)
            })
            .or_else(|| {
                self.active_agent()
                    .and_then(|pane| pane.read(cx).working_directory(cx))
            })
            .or_else(|| get().working_dir.clone());

        self.panels.set_git_target(cwd, cx);
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

    /// Show the settings entry, opening it if needed, on its About page.
    pub(super) fn show_about(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.on_show_settings(&ShowSettings, window, cx);

        self.settings.select_about(cx);
    }

    /// Drop the settings surface after its edits have been saved successfully.
    /// Reached from every path that removes the settings entry.
    pub(super) fn retire_settings_workspace(&mut self) {
        self.settings.retire();
    }

    /// Leave the settings or a remote entry for a normal workspace. Every
    /// path that adds a local tab goes through this, so a new tab never
    /// breaks the settings entry's single-tab presentation, and a tab running
    /// on this computer never sits among a host's tabs.
    pub(crate) fn leave_pseudo_workspace(&mut self) {
        if self.workspaces.active_kind() != WorkspaceKind::Normal {
            let index = self.workspaces.first_normal_index();

            self.workspaces.list_mut().activate(index);
        }
    }

    /// Keep background host events flowing while the pane is not in front.
    pub(crate) fn watch_pane(pane: &Entity<TerminalPane>, cx: &mut Context<Self>) {
        cx.observe(pane, |this, pane, cx| this.on_pane_notified(pane, cx))
            .detach();

        pane.update(cx, |pane, _| pane.close_tab_with(close_tab()));

        ui::remote::share_tab(pane, cx);

        cx.subscribe(pane, Self::on_agent_interrupted).detach();
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

        apply_monitor_display_change(&mutation, cx);

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
                        // The saved session includes the title, so a restore
                        // labels the tab with it before its shell runs.
                        let changed = tabs.set_title(tab_id, title.clone());

                        ui::remote::tab_title_changed(&pane, title, cx);

                        chrome_changed |= changed;
                        session_changed |= changed;
                    }
                }
                HostEvent::Exit => {
                    self.agent_notifications.remove_route(&agent_route, cx);

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
                // in the chrome, not in the pane's own grid.
                HostEvent::AltScreen(_)
                | HostEvent::PromptBoundaryTrusted(_)
                | HostEvent::PromptStarted
                | HostEvent::CommandStarted => chrome_changed = true,
                HostEvent::Cwd(_) => session_changed = true,
                HostEvent::Notification { title, body } => {
                    let mutation =
                        self.agent_notifications
                            .agent_monitor
                            .notify(&agent_route, title, body);

                    remove_native_notifications(&mutation.removed_notifications);

                    chrome_changed |= mutation.visible_changed;

                    self.process_native_notifications(cx);
                }
                _ => {}
            }
        }

        if session_changed {
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

        open_workspace_dirs_dialog(id, roots, window, cx);
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

    pub(super) fn exact_window_active(window: &Window) -> bool {
        native_active_state(window).unwrap_or_else(|| window.is_window_active())
    }

    /// Offer the monitor's pending notifications to the system, keeping back
    /// the one for the agent the user is looking at right now.
    pub(super) fn process_native_notifications(&mut self, cx: &mut Context<Self>) {
        let visible_route = self
            .window_active
            .then(|| self.active_agent_route(cx))
            .flatten();

        self.agent_notifications
            .process_native_notifications(visible_route.as_ref(), cx);
    }

    fn owns_agent_route(&self, route: &AgentRoute, cx: &App) -> bool {
        self.workspaces.all_tabs().any(|tabs| {
            tabs.list().items().iter().any(|tab| {
                tab.surface()
                    .agent_routes(cx)
                    .iter()
                    .any(|candidate| candidate == route)
            })
        })
    }

    fn locate_agent_route(&self, route: &AgentRoute, cx: &App) -> Option<AgentRouteLocation> {
        for (workspace_index, summary) in self.workspaces.summaries().iter().enumerate() {
            let tabs = self.workspaces.tabs_of(summary.id)?;

            for (tab_index, tab) in tabs.list().items().iter().enumerate() {
                if tab.surface().agent().is_some()
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
                        terminal_pane_id: None,
                    });
                }

                for (pane_id, pane) in tab.surface().leaves() {
                    if pane.read(cx).agent_route() == route {
                        return Some(AgentRouteLocation {
                            workspace_id: summary.id,
                            workspace_index,
                            tab_id: tab.id(),
                            tab_index,
                            terminal_pane_id: Some(pane_id),
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
        // A delivered notification can be clicked after it was acknowledged
        // or replaced. Its route still identifies the tab while that tab lives.
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

        if let Some(pane_id) = location.terminal_pane_id {
            self.workspaces
                .active_tabs_mut()
                .active_mut()
                .live_mut()
                .tree_mut()
                .set_focused(pane_id);
        }

        self.show_active_tab(window, cx);

        self.agent_notifications
            .acknowledge(route, notification_id, cx);

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

        apply_monitor_display_change(&mutation, cx);

        self.agent_notifications.reschedule_agent_timer(cx);

        self.process_native_notifications(cx);

        true
    }

    fn process_due_agent_deadlines(&mut self, generation: u64, cx: &mut Context<Self>) {
        let Some(mutation) = self.agent_notifications.process_due(generation) else {
            return;
        };

        apply_monitor_display_change(&mutation, cx);

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

    /// `shared` is the id paired devices knew a restored tab by.
    pub(crate) fn watch_agent_tab(
        pane: &Entity<AgentPane>,
        shared: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = pane.read(cx).agent_session() else {
            return;
        };

        cx.subscribe(&session, Self::on_agent_pane_event).detach();

        pane.update(cx, |pane, _| pane.close_tab_with(close_tab()));

        ui::remote::share_agent_tab(pane, shared, cx);
    }

    /// Start the still-pending tab that paired devices know by `id`, in
    /// place and without activating it, because a device asked for the
    /// agent or terminal session it holds. Returns whether such a tab was
    /// started.
    pub(crate) fn start_pending_tab(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(tab) = self
            .workspaces
            .find_tab_id(|surface| surface.pending_session() == Some(id))
        else {
            return false;
        };

        let restored = materialize_tab(&mut self.workspaces, tab, &mut self.next_id, window, cx);

        cx.notify();

        restored
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
                cx.notify();

                return;
            }
            // The Side Chat control reads the pane at render time, so this
            // only has to repaint.
            AgentPaneEvent::SideChatActivity => {
                cx.notify();

                return;
            }
            // Addressed to the Team that owns the member's room; a member's
            // session is never a tab of its own.
            AgentPaneEvent::TeamPrompt(_) => return,
            AgentPaneEvent::ResumeElsewhere {
                cwd,
                summary,
                profile,
            } => {
                // Opening a tab needs a window, which an event
                // subscription has none of; the next render has one.
                self.agent_notifications.pending_agent_resume = Some(PendingAgentResume {
                    profile: profile
                        .clone()
                        .unwrap_or_else(|| session.read(cx).profile().clone()),
                    cwd: cwd.clone(),
                    fallback: session.read(cx).workspace().clone(),
                    summary: summary.clone(),
                });

                cx.notify();

                return;
            }
            AgentPaneEvent::TitleSuggested(title) => {
                // A user-authored rename outranks a suggestion, so a tab the
                // user has named keeps its name. An agent tab's rename names
                // the conversation, though, so an empty suggestion (the
                // conversation was replaced) drops it along with the old
                // conversation.
                if let Some(tab_id) = self.tab_for_agent_session(&session)
                    && let Some(tabs) = self.workspaces.tabs_for_tab_mut(tab_id)
                    && (title.is_empty() && tabs.clear_rename(tab_id))
                        | tabs.set_title(tab_id, title.clone())
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
            // Addressed to paired devices; this window notifies from the
            // lifecycle event that accompanies it.
            AgentPaneEvent::Attention { .. } => return,
            AgentPaneEvent::SwitchProfile { profile, resume } => {
                // Only a pick from the composer shows which agent the user
                // wants next time; continuing another agent's conversation
                // does not.
                if resume.is_none() {
                    cx.set_global(PickedAgentProfile(Some(profile.name.clone())));
                }

                // Same reason as the resume above: building the replacement
                // pane needs a window, and the next render has one.
                if let Some(tab) = self.tab_for_agent_session(&session) {
                    self.agent_notifications.pending_agent_switch = Some(PendingAgentSwitch {
                        tab,
                        profile: profile.clone(),
                        resume: resume.clone(),
                    });

                    cx.notify();
                }

                return;
            }
        };

        apply_monitor_display_change(&mutation, cx);

        self.agent_notifications.reschedule_agent_timer(cx);

        self.process_native_notifications(cx);
    }

    fn bind_actions(element: Div, cx: &mut Context<Self>) -> Div {
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
            .on_action(cx.listener(Self::on_quote_git_line))
            .on_action(cx.listener(Self::on_return_from_git))
            .on_action(cx.listener(Self::on_toggle_background_tasks))
            .on_action(cx.listener(Self::on_show_settings))
            .on_action(cx.listener(Self::on_new_agent_tab))
            .on_action(cx.listener(Self::on_new_team_tab))
            .on_action(cx.listener(Self::on_toggle_workflows))
    }

    /// What the title bar shows for the window's current state.
    fn title_bar_inputs(&self, tab_strip: Option<AnyElement>, cx: &App) -> TitleBarInputs {
        // Both panel toggles count the work of the active tab, because
        // activating either one opens that tab's runs or children.
        let active_agent = self.active_agent();

        TitleBarInputs {
            sidebar_width: match self.sidebar.collapsed {
                true => 0.0,
                false => self.sidebar.width,
            },
            sidebar_collapsed: self.sidebar.collapsed,
            tab_strip,
            has_ready_tab: self.next_ready_tab(cx).is_some(),
            has_busy_tab: self.next_busy_tab(cx).is_some(),
            git_tab_active: self.workspaces.active_tabs().active().is_git(),
            // The workflow control stays out of the chrome until a run exists
            // to look at.
            workflows: self.panels.workflows_seen().then(|| PanelToggle {
                running: active_agent
                    .as_ref()
                    .map(|pane| pane.read(cx).running_workflow_agents())
                    .unwrap_or(0),
                open: self.panels.shows(RightPanelKind::Workflows, cx),
            }),
            // Finished children remain reachable in their own session without
            // making the control appear in tabs that have no children.
            background_tasks: active_agent
                .filter(|pane| pane.read(cx).background_task_count() > 0)
                .map(|pane| PanelToggle {
                    running: pane.read(cx).running_background_tasks(),
                    open: self.panels.shows(RightPanelKind::BackgroundTasks, cx),
                }),
            side_chat: self
                .active_agent()
                .and_then(|pane| pane.read(cx).side_chat_shown()),
        }
    }

    /// Minimize the active tab's Side Chat window, or bring it back.
    pub(super) fn on_toggle_side_chat(&mut self, cx: &mut Context<Self>) {
        if let Some(pane) = self.active_agent() {
            pane.update(cx, |pane, cx| pane.toggle_side_chat(cx));
        }
    }

    /// Publish this window's session to the registry the app writes out on
    /// quit and keeps for reopening a closed window. Both readers take the
    /// snapshot at their own moment, when the window closes and when the app
    /// quits, so nothing in between has to keep the registry current.
    pub(crate) fn sync_session_memory(&self, cx: &mut Context<AppWindow>) {
        let session = session_state(&self.workspaces, self.doomed_workspace, cx);

        if let Some(entry) = cx.global_mut::<WindowRegistry>().get_mut(self.window_id) {
            entry.session = Some(session);
        }
    }

    pub(super) fn tab_strip_mut(&mut self) -> &mut TabStrip {
        &mut self.chrome.tab_strip
    }

    pub(super) fn vertical_tabs_mut(&mut self) -> &mut VerticalTabList {
        &mut self.chrome.vertical_tabs
    }
}

/// Walk from just after `active` and wrap around, returning the first marked
/// slot. `active` itself is visited last, so a tab that gets marked while it is
/// the one on screen stays reachable instead of being skipped forever.
pub(super) fn next_marked_position(marks: &[bool], active: usize) -> Option<usize> {
    (1..=marks.len())
        .map(|offset| (active + offset) % marks.len())
        .find(|&index| marks[index])
}

struct AgentRouteLocation {
    workspace_id: WorkspaceId,
    workspace_index: usize,
    tab_id: TabId,
    tab_index: usize,
    terminal_pane_id: Option<PaneId>,
}

impl Render for AppWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Restore lazily loaded tabs before rendering. Hibernated tabs keep
        // their saved state until the wake button is selected.
        self.ensure_active_tab_live(window, cx);

        self.window_active = Self::exact_window_active(window);

        self.acknowledge_visible(window, false, cx);

        window.set_window_title(&self.active_tab_title());

        ui::remote::sync_workspaces(self.window_id, &self.workspaces, cx);

        if self.chrome.needs_focus {
            self.chrome.needs_focus = false;

            self.on_active_tab_changed(window, cx);

            self.focus_active(window, cx);
        }

        if let Some(request) = self.agent_notifications.pending_agent_resume.take() {
            // The conversation ran in a directory of its own. Where a
            // workspace owns that directory, the reopened tab gets that
            // workspace's whole directory list; otherwise the conversation's
            // own directory is the only one this tab can claim with certainty.
            let workspace = match request.cwd {
                Some(cwd) => exact_match(&self.workspaces.summaries(), path::Path::new(&cwd))
                    .and_then(|id| self.workspaces.roots_of(id))
                    .map_or_else(
                        || AgentWorkspace::single(Some(cwd.clone())),
                        |roots| agent_workspace(Some(roots)),
                    ),
                None => request.fallback,
            };

            self.open_agent_tab_in(
                &request.profile,
                workspace,
                Some(request.summary),
                window,
                cx,
            );
        }

        if let Some(request) = self.agent_notifications.pending_agent_switch.take() {
            self.replace_agent_tab(request, window, cx);
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

        let sidebar_width = if self.sidebar.collapsed {
            0.0
        } else {
            self.sidebar.width
        };

        let content_width = window.viewport_size().width - px(sidebar_width);

        self.panels.panel().update(cx, |panel, cx| {
            panel.set_available_width(content_width, cx);
        });

        // The sidebar is always mounted so it can animate its width open/closed.
        let summaries = self.workspace_chrome(cx);

        // Vertical style folds the tab strip into the sidebar as child rows of
        // each workspace, leaving the title bar's strip slot empty.
        let vertical_tabs =
            cx.global::<AppSettings>().config().appearance.tab_bar_style == TabBarStyle::Vertical;

        let (unread_tabs, busy_agent_tabs) = self.tab_agent_indicators(cx);

        let tab_rows: Vec<Vec<AnyElement>> = match vertical_tabs {
            false => Vec::new(),
            true => {
                self.chrome.vertical_tabs.end_cancelled_drag(cx);

                let row_width = self.sidebar.tab_row_width();

                summaries
                    .iter()
                    .map(|ws| {
                        let Some(tabs) = self.workspaces.tabs_of(ws.summary.id) else {
                            return Vec::new();
                        };

                        self.chrome.vertical_tabs.render(
                            WorkspaceTabs {
                                index: ws.index,
                                tabs,
                                active: ws.summary.active,
                                closeable: ws.summary.closeable,
                                fold: ws.summary.tab_fold,
                            },
                            &unread_tabs,
                            &busy_agent_tabs,
                            &self.renames,
                            row_width,
                            window,
                            cx,
                        )
                    })
                    .collect()
            }
        };

        // A remote entry in front shows one session of its host, which that
        // host's list marks the way a tab row marks the tab on screen.
        let in_front = (self.workspaces.active_kind() == WorkspaceKind::Remote)
            .then(|| self.workspaces.active_tabs().active());

        let remote = cx
            .global::<Remote>()
            .remote_workspaces(|host| self.remote_workspaces.contains_key(host.as_str()))
            .into_iter()
            .map(|mut host| {
                host.selected = in_front.and_then(|surface| {
                    host.sessions
                        .iter()
                        .find(|session| {
                            surface.follows_remote(host.id.as_str(), &session.session, cx)
                        })
                        .map(|session| session.session.clone())
                });

                host.followed = host
                    .sessions
                    .iter()
                    .filter(|session| {
                        self.workspaces
                            .find_tab_id(|surface| {
                                surface.follows_remote(host.id.as_str(), &session.session, cx)
                            })
                            .is_some()
                    })
                    .map(|session| session.session.clone())
                    .collect();

                host
            })
            .collect();

        let sidebar = self.sidebar.render(
            summaries,
            tab_rows,
            &self.renames,
            SidebarUsage {
                daily: self.chrome.token_usage.clone(),
                quotas: self.chrome.agent_usage.clone(),
            },
            remote,
            window,
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

        let tab_strip = (!vertical_tabs).then(|| {
            self.chrome.tab_strip.render(
                self.workspaces.active_tabs(),
                &unread_tabs,
                &busy_agent_tabs,
                &self.renames,
                cx,
            )
        });

        let title_bar = self
            .chrome
            .title_bar
            .render(self.title_bar_inputs(tab_strip, cx), cx);

        self.apply_pending_ratios(cx);

        let pane_tree = tab_surface_view(
            self.workspaces.active_tabs().active(),
            active_id,
            self.active_agent(),
            self.settings.render(cx),
            cx,
        );

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
            // that this window keeps its focused backdrop material, which also
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
            // white/transparent), and the chrome now has see-through regions
            // (the tab strip and the gutters around the terminal cards), so the
            // shell paints the chrome background across the whole window.
            // `apply_window_translucency` dims this color with the rest of the
            // chrome when window transparency is on.
            .bg(cx.theme().background)
            .flex()
            .flex_col()
            // All chrome inherits the configured UI font; terminal panes override it.
            .font(ui::chrome_font(cx))
            .when(
                matches!(
                    self.workspaces.active_tabs().active(),
                    TabSurface::Hibernated(_)
                ),
                |shell| shell.track_focus(&self.focus),
            )
            .key_context("AppWindow");

        Self::bind_actions(shell, cx)
            .children(background_image)
            .child(title_bar)
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
                            .child(
                                floating_surface_card(self.workspaces.active_tabs().active(), cx)
                                    .id("main-floating-surface")
                                    .min_w_0()
                                    .relative()
                                    .child(pane_tree)
                                    .child(surface_border(cx))
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

impl Focusable for AppWindow {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

const PANE_RESIZE_STEP: Pixels = px(30.0);

impl AppWindow {
    pub(super) fn on_toggle_git_sidebar(
        &mut self,
        _: &ToggleGitSidebar,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.leave_pseudo_workspace();

        if self.workspaces.active_tabs().active().is_git() {
            let id = self.workspaces.active_tabs().list().active_id();

            self.request_close_tab(id, window, cx);

            return;
        }

        let tabs = self.workspaces.active_tabs();
        let return_to = (!tabs.active().is_git()).then_some(tabs.list().active_id());

        let existing = tabs
            .list()
            .items()
            .iter()
            .position(|tab| tab.surface().is_git());

        if let Some(index) = existing {
            self.workspaces.active_tabs_mut().list_mut().activate(index);
            self.ensure_active_tab_live(window, cx);

            if let TabSurface::Git(tab) = self.workspaces.active_tabs_mut().active_mut()
                && return_to.is_some()
            {
                tab.return_to = return_to;
            }
        } else {
            let cwd = self
                .try_active_pane()
                .and_then(|pane| pane.read(cx).tab_state().cwd)
                .or_else(|| {
                    self.active_agent()
                        .and_then(|pane| pane.read(cx).working_directory(cx))
                })
                .or_else(|| {
                    self.workspaces
                        .active_roots()
                        .map(|roots| roots.primary().to_string())
                })
                .unwrap_or_default();

            let view = cx.new(|cx| GitSidebar::new(cwd, window, cx));
            let id = TabId(Self::alloc_id(&mut self.next_id));

            self.workspaces.active_tabs_mut().new_tab(
                TabSurface::Git(GitTab { view, return_to }),
                id,
                t!("git-tab-title").into_owned(),
            );
        }

        self.show_active_tab(window, cx);
    }

    pub(super) fn sync_git_tab_visibility(&self, cx: &mut Context<Self>) {
        let active = self.workspaces.active_tabs().list().active_id();

        let can_quote = self
            .workspaces
            .active_tabs()
            .list()
            .items()
            .iter()
            .any(|tab| tab.surface().is_agent());

        let views: Vec<_> = self
            .workspaces
            .all_tabs()
            .flat_map(|tabs| tabs.list().items())
            .filter_map(|tab| {
                tab.surface()
                    .git()
                    .map(|git| (tab.id() == active, git.view.clone()))
            })
            .collect();

        for (visible, view) in views {
            view.update(cx, |view, cx| {
                view.set_quote_available(visible && can_quote, cx);
                view.set_visible(visible, cx);
            });
        }
    }

    pub(super) fn on_return_from_git(
        &mut self,
        _: &ReturnFromGit,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(git) = self.workspaces.active_tabs().active().git() else {
            return;
        };

        let preferred = git.return_to;
        let tabs = self.workspaces.active_tabs();

        let index = preferred
            .and_then(|id| tabs.list().items().iter().position(|tab| tab.id() == id))
            .or_else(|| {
                tabs.list()
                    .items()
                    .iter()
                    .position(|tab| !tab.surface().is_git())
            });

        if let Some(index) = index {
            self.workspaces.active_tabs_mut().list_mut().activate(index);
            self.show_active_tab(window, cx);
        }
    }

    pub(super) fn on_quote_git_line(
        &mut self,
        _: &QuoteGitLine,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(git) = self.workspaces.active_tabs().active().git() else {
            return;
        };

        let Some(reference) = git.view.read(cx).selected_reference(cx) else {
            return;
        };

        let preferred = git.return_to;
        let tabs = self.workspaces.active_tabs();

        let index = tabs
            .list()
            .items()
            .iter()
            .position(|tab| Some(tab.id()) == preferred && tab.surface().is_agent())
            .or_else(|| {
                tabs.list()
                    .items()
                    .iter()
                    .position(|tab| tab.surface().is_agent())
            });

        let Some(index) = index else { return };

        self.workspaces.active_tabs_mut().list_mut().activate(index);
        self.on_active_tab_changed(window, cx);

        if let Some(agent) = self.active_agent() {
            agent.update(cx, |pane, cx| {
                pane.append_code_reference(&reference, window, cx)
            });
        }

        self.focus_active(window, cx);

        cx.notify();
    }
}
