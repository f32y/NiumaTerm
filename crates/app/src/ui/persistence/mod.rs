//! A window's saved session: rebuilding it on launch, and the starting
//! session of a window with none.

pub(super) use crate::ui::persistence::snapshot::session_state;

mod snapshot;

#[cfg(test)]
mod tests;

use app::agent_tab::execution::AgentSession;
use app::agent_tab::team::{TeamPane, TeamRuntime};
use app::agent_tab::{AgentKind, AgentPane};
use app::terminal_tab::view::TerminalPane;
use dirs::home_dir;
use gpui::{App, AppContext, Axis, Context, Entity, Window};
use gpui_component::resizable::ResizableState;
use nmt_agent::team::identity::RoomId;
use nmt_config::config_dir_path;
use nmt_config::local_state::{
    PaneNodeState, PaneSplitAxis, SessionState, TabState, WorkspaceState,
};
use rust_i18n::t;
use tracing::warn;

use crate::pane_tree::{PaneId, PaneNode, PaneTree};
use crate::tabs::{TabId, TabManager};
use crate::ui::AppWindow;
use crate::ui::git_sidebar::GitSidebar;
use crate::ui::settings::{AgentProfile, AppSettings, builtin_agent_profile};
use crate::ui::shell::tab_surface::{AgentTab, GitTab, TerminalPaneTree};
use crate::ui::shell::{TabSurface, agent_workspace};
use crate::ui::terminal_launch::{launch_with_profile, spawn_default_pane, spawn_pane};
use crate::ui::terminal_layout::TerminalLayout;
use crate::workspace::{WorkspaceId, WorkspaceManager, WorkspaceRoots, default_workspace_name};

/// What a saved tab reopens as.
enum SavedTab<'a> {
    /// A git review of this directory.
    Git(&'a str),
    /// The team room with this saved id.
    Team(&'a str),
    /// A fresh conversation of this agent kind.
    Agent(AgentKind),
    /// A terminal. An agent kind this build does not know (a newer snapshot)
    /// degrades to a terminal rather than losing the tab.
    Terminal,
}

fn saved_tab(state: &TabState) -> SavedTab<'_> {
    if let Some(cwd) = state.git_cwd.as_deref() {
        return SavedTab::Git(cwd);
    }

    if let Some(room) = state.team_room.as_deref() {
        return SavedTab::Team(room);
    }

    match state.agent.as_deref().and_then(AgentKind::from_id) {
        Some(kind) => SavedTab::Agent(kind),
        None => SavedTab::Terminal,
    }
}

/// Older snapshots stored generated labels as if they were user names. The new
/// flag is absent there, so the exact built-in label shape is the compatibility
/// signal that lets OSC titles work immediately after upgrading.
fn legacy_generated_tab_title(title: &str) -> bool {
    title
        .strip_prefix("Tab ")
        .is_some_and(|number| number.parse::<usize>().is_ok())
}

/// Resolve a saved launch command for restore: a `None` shell follows the current
/// default profile, and a shell matching no configured profile is a stale pin
/// (e.g. a former built-in default the user has since moved away from) that falls
/// back to the default profile as well. Only a shell still present in the profile
/// list keeps its saved command.
fn resolve_restored_launch(state: &mut TabState, settings: &AppSettings) {
    let keep = state.shell.as_deref().is_some_and(|shell| {
        settings
            .config()
            .profiles
            .list
            .iter()
            .any(|p| p.shell.trim().eq_ignore_ascii_case(shell))
    });

    if !keep {
        let (shell, args) = settings.default_profile_command();

        state.shell = shell;
        state.args = args;
    }
}

/// Resolve a restored agent tab's launch profile: the saved name when it
/// still exists, then the first configured profile of the same kind, then
/// the built-in profile — so the tab always reopens even after the profile
/// it was created from was renamed or deleted.
fn restored_agent_profile(
    name: Option<&str>,
    kind: AgentKind,
    settings: &AppSettings,
) -> AgentProfile {
    settings
        .config()
        .agent_profiles
        .list
        .iter()
        .find(|p| name.is_some_and(|name| p.name == name))
        .or_else(|| {
            settings
                .config()
                .agent_profiles
                .list
                .iter()
                .find(|p| p.kind == kind)
        })
        .cloned()
        .unwrap_or_else(|| builtin_agent_profile(kind))
}

fn axis_from_state(axis: PaneSplitAxis) -> Axis {
    match axis {
        PaneSplitAxis::Horizontal => Axis::Horizontal,
        PaneSplitAxis::Vertical => Axis::Vertical,
    }
}

/// The starting workspace set for a window without a restored session:
/// one workspace, one tab. With `initial_cwd` (a CLI `new_window` target)
/// the workspace is rooted there; otherwise it uses the home directory.
pub(super) fn default_session(
    initial_cwd: Option<String>,
    default_profile: (Option<String>, Vec<String>),
    next_id: &mut u64,
    cx: &mut Context<AppWindow>,
) -> WorkspaceManager {
    // The default (no-CLI) branch keeps spawning with no cwd — the shell
    // then starts in its own default directory, as before.
    let (cwd, spawn_cwd) = match initial_cwd {
        Some(dir) => (dir.clone(), Some(dir)),
        None => (
            home_dir()
                .map(|home| home.display().to_string())
                .unwrap_or_else(|| ".".to_string()),
            None,
        ),
    };

    let surface_id = AppWindow::alloc_id(next_id);
    let pane = spawn_default_pane(cx, surface_id, default_profile, spawn_cwd);
    let title = pane.read(cx).profile_name().to_string();

    let tabs = TabManager::new(
        TabSurface::Live(TerminalLayout::new_leaf(PaneId(surface_id), pane)),
        TabId(surface_id),
        title,
    );

    let workspace_id = AppWindow::alloc_id(next_id);

    WorkspaceManager::new(
        tabs,
        WorkspaceId(workspace_id),
        default_workspace_name().into(),
        WorkspaceRoots::single(cwd),
    )
}

pub(super) fn restore_session(
    session: Option<SessionState>,
    next_id: &mut u64,
    window: &mut Window,
    cx: &mut Context<AppWindow>,
) -> Option<WorkspaceManager> {
    let session = session?;
    let saved_active = session.active_workspace;

    let mut workspaces: Option<WorkspaceManager> = None;
    let mut restored_count = 0usize;

    for workspace in session.workspaces {
        let WorkspaceState {
            name,
            cwd,
            additional_cwds,
            pinned,
            active_tab,
            tabs,
        } = workspace;

        let Some(tab_manager) = restore_tabs(tabs, active_tab, next_id, cx) else {
            continue;
        };

        restored_count += 1;

        let workspace_id = WorkspaceId(AppWindow::alloc_id(next_id));

        let name = if name.trim().is_empty() {
            t!("workspace-restored-default-name", count = restored_count).into_owned()
        } else {
            name
        };

        let cwd = cwd
            .filter(|cwd| !cwd.trim().is_empty())
            .unwrap_or_else(|| ".".to_string());

        // A saved directory restores whether or not it currently resolves:
        // a disconnected drive or a temporarily missing tree would
        // otherwise silently drop the workspace's own tabs and layout. The
        // sidebar marks what it cannot reach.
        let roots = WorkspaceRoots::new(
            cwd,
            additional_cwds
                .into_iter()
                .filter(|cwd| !cwd.trim().is_empty())
                .collect(),
        );

        if let Some(manager) = &mut workspaces {
            manager.new_workspace_with_pinned(tab_manager, workspace_id, name, roots, pinned);
        } else {
            workspaces = Some(WorkspaceManager::new(
                tab_manager,
                workspace_id,
                name,
                roots,
            ));

            workspaces
                .as_mut()
                .expect("workspace manager was just created")
                .set_pinned(workspace_id, pinned);
        }
    }

    let mut workspaces = workspaces?;

    let active_index = saved_active.min(workspaces.list().len() - 1);

    workspaces.list_mut().activate(active_index);

    // The initially visible tab spawns right away; everything else stays
    // pending, so this window's other `active_pane` readers (activation
    // observers, notification pumps) never see a pending active tab.
    materialize_active_tab(&mut workspaces, next_id, window, cx);

    Some(workspaces)
}

/// Spawn the shells of a still-pending active tab and swap its surface to
/// `Live`. Returns whether a materialization happened. A saved pane layout
/// rebuilds the split tree (one fresh shell per leaf); an unusable layout
/// or failed spawn degrades to a single default-profile pane so the tab
/// the user just activated never vanishes. A tab saved with an agent kind
/// reopens as a fresh agent conversation instead (nothing to restore —
/// the Codex process died with the previous app instance).
pub(super) fn materialize_active_tab(
    workspaces: &mut WorkspaceManager,
    next_id: &mut u64,
    window: &mut Window,
    cx: &mut Context<AppWindow>,
) -> bool {
    let state = match workspaces.active_tabs().active() {
        TabSurface::Pending(state) => (**state).clone(),
        TabSurface::TeamDisabled(state)
            if cx.global::<AppSettings>().config().agent.enable_agent_team =>
        {
            (**state).clone()
        }
        TabSurface::Live(_)
        | TabSurface::Agent(_)
        | TabSurface::Git(_)
        | TabSurface::Settings
        | TabSurface::Team(_)
        | TabSurface::TeamDisabled(_)
        | TabSurface::TeamUnavailable { .. } => return false,
    };

    let surface = match saved_tab(&state) {
        SavedTab::Git(cwd) => {
            let view = cx.new(|cx| GitSidebar::new(cwd.to_owned(), window, cx));

            TabSurface::Git(GitTab {
                view,
                return_to: None,
            })
        }
        SavedTab::Team(saved_id) => restore_team_tab(saved_id, &state, window, cx),
        SavedTab::Agent(kind) => {
            let workspace = agent_workspace(workspaces.active_roots());

            let profile = restored_agent_profile(
                state.agent_profile.as_deref(),
                kind,
                cx.global::<AppSettings>(),
            );

            let owner = AgentSession::create(profile, workspace, None, cx);
            let pane = cx.new(|cx| AgentPane::attach(&owner, window, cx));

            AppWindow::watch_agent_tab(&pane, cx);

            owner.start(None, cx);

            TabSurface::Agent(AgentTab { owner, pane })
        }
        SavedTab::Terminal => TabSurface::Live(restore_terminal_tree(state, next_id, cx)),
    };

    *workspaces.active_tabs_mut().active_mut() = surface;

    true
}

/// A saved terminal tab's pane layout, rebuilt with one fresh shell per leaf.
/// An unusable layout, or a single pane that fails to spawn, degrades to one
/// default-profile pane.
fn restore_terminal_tree(
    state: TabState,
    next_id: &mut u64,
    cx: &mut Context<AppWindow>,
) -> TerminalPaneTree {
    state
        .panes
        .as_ref()
        .and_then(|panes| restore_pane_node(panes, next_id, cx))
        .map(Into::into)
        .unwrap_or_else(|| {
            let surface_id = AppWindow::alloc_id(next_id);
            let default_profile = cx.global::<AppSettings>().default_profile_command();

            let (launch, profile_name) =
                launch_with_profile(Some(state), default_profile.clone(), cx);

            let pane = match spawn_pane(cx, surface_id, launch, profile_name) {
                Ok(pane) => {
                    AppWindow::watch_pane(&pane, cx);

                    pane
                }
                Err(error) => {
                    warn!("failed to restore tab {surface_id} lazily: {error}");

                    spawn_default_pane(cx, surface_id, default_profile, None)
                }
            };

            TerminalLayout::new_leaf(PaneId(surface_id), pane)
        })
}

fn restore_team_tab(
    saved_id: &str,
    state: &TabState,
    window: &mut Window,
    cx: &mut App,
) -> TabSurface {
    if !cx.global::<AppSettings>().config().agent.enable_agent_team {
        return TabSurface::TeamDisabled(Box::new(state.clone()));
    }

    let restored = saved_id
        .parse::<RoomId>()
        .map_err(|error| error.to_string())
        .and_then(|id| {
            TeamRuntime::open(&config_dir_path(), id, cx).map_err(|error| error.to_string())
        });

    match restored {
        Ok(runtime) => TabSurface::Team(cx.new(|cx| TeamPane::new(runtime, window, cx))),
        Err(message) => TabSurface::TeamUnavailable {
            saved: Box::new(state.clone()),
            message,
        },
    }
}

/// Rebuild a workspace's tabs as pending surfaces: the saved snapshot is
/// kept per tab and no shell spawns here — `materialize_active_tab` turns
/// a tab live the first time it is activated.
fn restore_tabs(
    tabs: Vec<TabState>,
    active_tab: usize,
    next_id: &mut u64,
    cx: &App,
) -> Option<TabManager<TabSurface>> {
    let mut restored = Vec::new();

    for mut tab_state in tabs {
        // Agent tabs carry no launch command; profile resolution only
        // applies to terminal tabs.
        if matches!(saved_tab(&tab_state), SavedTab::Terminal) {
            resolve_restored_launch(&mut tab_state, cx.global::<AppSettings>());
        }

        let name = tab_state
            .name
            .clone()
            .filter(|n| !n.trim().is_empty())
            .filter(|n| tab_state.user_named || !legacy_generated_tab_title(n));

        // The profile-derived title a live pane would report, so pending
        // tabs label identically to spawned ones.
        let default_title = match saved_tab(&tab_state) {
            SavedTab::Git(_) => t!("git-tab-title").into_owned(),
            SavedTab::Team(_) => t!("team-title").into_owned(),
            SavedTab::Agent(kind) => {
                let name = restored_agent_profile(
                    tab_state.agent_profile.as_deref(),
                    kind,
                    cx.global::<AppSettings>(),
                )
                .name;

                if name.trim().is_empty() {
                    kind.display().to_string()
                } else {
                    name
                }
            }
            SavedTab::Terminal => cx
                .global::<AppSettings>()
                .profile_name_for_command(tab_state.shell.as_deref(), &tab_state.args),
        };

        restored.push((
            TabSurface::Pending(Box::new(tab_state)),
            TabId(AppWindow::alloc_id(next_id)),
            name,
            default_title,
        ));
    }

    let mut restored = restored.into_iter();

    let (first_pane, first_id, first_name, first_default_title) = restored.next()?;

    let mut tab_manager = TabManager::new(first_pane, first_id, first_default_title);

    if let Some(name) = first_name {
        tab_manager.rename(first_id, name);
    }

    for (pane, id, name, default_title) in restored {
        tab_manager.new_tab(pane, id, default_title);

        if let Some(name) = name {
            tab_manager.rename(id, name);
        }
    }

    let active_index = active_tab.min(tab_manager.list().len() - 1);

    tab_manager.list_mut().activate(active_index);

    Some(tab_manager)
}

/// Rebuild one node of a saved pane layout, spawning a fresh shell per
/// leaf. An unspawnable leaf is skipped and its split collapses around it
/// (a split left with one child becomes that child); `None` when no leaf
/// of the subtree could spawn.
fn restore_pane_node(
    node: &PaneNodeState,
    next_id: &mut u64,
    cx: &mut Context<AppWindow>,
) -> Option<PaneNode<Entity<TerminalPane>>> {
    match node {
        PaneNodeState::Leaf {
            shell,
            args,
            cwd,
            grid_size,
        } => {
            let surface_id = AppWindow::alloc_id(next_id);

            let mut launch = TabState {
                shell: shell.clone(),
                args: args.clone(),
                cwd: cwd.clone(),
                grid_size: *grid_size,
                ..TabState::default()
            };

            resolve_restored_launch(&mut launch, cx.global::<AppSettings>());

            let (launch, profile_name) = launch_with_profile(Some(launch), (None, Vec::new()), cx);

            match spawn_pane(cx, surface_id, launch, profile_name) {
                Ok(pane) => {
                    AppWindow::watch_pane(&pane, cx);

                    Some(PaneTree::restored_leaf(PaneId(surface_id), pane))
                }
                Err(error) => {
                    warn!("failed to restore pane {surface_id}: {error}");

                    None
                }
            }
        }
        PaneNodeState::Split {
            axis,
            ratios,
            children,
        } => {
            let built: Vec<_> = children
                .iter()
                .filter_map(|child| restore_pane_node(child, next_id, cx))
                .collect();

            match built.len() {
                0 => None,
                1 => built.into_iter().next(),
                _ => {
                    let state = cx.new(|_| ResizableState::default());

                    // `restored_split` drops the ratios when their length
                    // no longer matches (a leaf was skipped).
                    Some(PaneTree::restored_split(
                        axis_from_state(*axis),
                        built,
                        state,
                        Some(ratios.clone()),
                    ))
                }
            }
        }
    }
}
