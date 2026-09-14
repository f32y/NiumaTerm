use std::process;

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
use crate::ui::Shell;
use crate::ui::git_sidebar::GitSidebar;
use crate::ui::settings::{AgentProfile, AppSettings, builtin_agent_profile};
use crate::ui::shell::tab_surface::{AgentTab, GitTab};
use crate::ui::shell::{TabSurface, agent_workspace};
use crate::ui::terminal_launch::spawn_pane;
use crate::ui::terminal_layout::TerminalLayout;
use crate::workspace::{
    WorkspaceId, WorkspaceKind, WorkspaceManager, WorkspaceRoots, default_workspace_name,
};

/// A pane that runs the current default profile's exact command is saved with
/// `shell = None` — "follow the default profile" — so later profile changes apply
/// to restored sessions instead of pinning today's shell path forever.
fn normalize_saved_launch(state: &mut TabState, default_profile: &(Option<String>, Vec<String>)) {
    if state.shell == default_profile.0 && state.args == default_profile.1 {
        state.shell = None;
        state.args = Vec::new();
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

/// Fill a launch's blank shell from the default profile and resolve the
/// display name of the profile it runs. A `None` shell means "follow the
/// default profile" (session persistence); resolving it here keeps the
/// hardcoded built-in fallback in the session layer from swallowing the
/// configured profile. The pane takes only the resolved values, so profile
/// policy stays with the settings that define it.
fn launch_with_profile(
    tab_state: Option<TabState>,
    default_profile: (Option<String>, Vec<String>),
    cx: &mut impl AppContext,
) -> (TabState, String) {
    let mut tab_state = tab_state.unwrap_or_default();

    if tab_state.shell.is_none() {
        tab_state.shell = default_profile.0;
        tab_state.args = default_profile.1;
    }

    let profile_name = cx.read_global(|settings: &AppSettings, _| {
        settings.profile_name_for_command(tab_state.shell.as_deref(), &tab_state.args)
    });

    (tab_state, profile_name)
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

fn axis_to_state(axis: Axis) -> PaneSplitAxis {
    match axis {
        Axis::Horizontal => PaneSplitAxis::Horizontal,
        Axis::Vertical => PaneSplitAxis::Vertical,
    }
}

fn axis_from_state(axis: PaneSplitAxis) -> Axis {
    match axis {
        PaneSplitAxis::Horizontal => Axis::Horizontal,
        PaneSplitAxis::Vertical => Axis::Vertical,
    }
}

/// Serialize a pane-tree node for the session snapshot: leaves carry their
/// live launch state (cwd tracks OSC 7), splits carry their axis and the
/// current panel sizes normalized to ratios.
fn pane_node_state(
    node: &PaneNode<Entity<TerminalPane>>,
    default_profile: &(Option<String>, Vec<String>),
    cx: &App,
) -> PaneNodeState {
    match node {
        PaneNode::Leaf { pane, .. } => {
            let mut state = pane.read(cx).tab_state();

            normalize_saved_launch(&mut state, default_profile);

            PaneNodeState::Leaf {
                shell: state.shell,
                args: state.args,
                cwd: state.cwd,
                grid_size: state.grid_size,
            }
        }
        PaneNode::Split {
            axis,
            children,
            state,
            ..
        } => {
            let sizes = state.read(cx).sizes().clone();
            let total: f32 = sizes.iter().map(|size| size.as_f32()).sum();

            let ratios = if sizes.len() == children.len() && total > 0.0 {
                sizes.iter().map(|size| size.as_f32() / total).collect()
            } else {
                // Sizes not laid out yet (tab never shown): equal split.
                vec![1.0 / children.len() as f32; children.len()]
            };

            PaneNodeState::Split {
                axis: axis_to_state(*axis),
                ratios,
                children: children
                    .iter()
                    .map(|c| pane_node_state(c, default_profile, cx))
                    .collect(),
            }
        }
    }
}

/// The starting workspace set for a window without a restored session:
/// one workspace, one tab. With `initial_cwd` (a CLI `new_window` target)
/// the workspace is rooted there; otherwise it uses the home directory.
pub(super) fn default_session(
    initial_cwd: Option<String>,
    default_profile: (Option<String>, Vec<String>),
    next_id: &mut u64,
    cx: &mut Context<Shell>,
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

    let surface_id = Shell::alloc_id(next_id);
    let pane = spawn_default_pane(cx, surface_id, default_profile, spawn_cwd);
    let title = pane.read(cx).profile_name().to_string();

    let tabs = TabManager::new(
        TabSurface::Live(TerminalLayout::new_leaf(PaneId(surface_id), pane)),
        TabId(surface_id),
        title,
    );

    let workspace_id = Shell::alloc_id(next_id);

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
    cx: &mut Context<Shell>,
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

        let workspace_id = WorkspaceId(Shell::alloc_id(next_id));

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
    cx: &mut Context<Shell>,
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

    if let Some(cwd) = &state.git_cwd {
        let view = cx.new(|cx| GitSidebar::new(cwd.clone(), window, cx));

        *workspaces.active_tabs_mut().active_mut() = TabSurface::Git(GitTab {
            view,
            return_to: None,
        });

        return true;
    }

    if let Some(saved_id) = state.team_room.as_deref() {
        *workspaces.active_tabs_mut().active_mut() = restore_team_tab(saved_id, &state, window, cx);

        return true;
    }

    // An unknown agent kind (a newer snapshot) degrades to the terminal
    // path below rather than losing the tab.
    if let Some(kind) = state.agent.as_deref().and_then(AgentKind::from_id) {
        let workspace = agent_workspace(workspaces.active_roots());

        let profile = restored_agent_profile(
            state.agent_profile.as_deref(),
            kind,
            cx.global::<AppSettings>(),
        );

        let owner = AgentSession::create(profile, workspace, None, cx);
        let pane = cx.new(|cx| AgentPane::attach(&owner, window, cx));

        Shell::watch_agent_tab(&pane, cx);

        owner.start(None, cx);

        *workspaces.active_tabs_mut().active_mut() = TabSurface::Agent(AgentTab { owner, pane });

        return true;
    }

    let tree = state
        .panes
        .as_ref()
        .and_then(|panes| restore_pane_node(panes, next_id, cx))
        .map(Into::into)
        .unwrap_or_else(|| {
            let surface_id = Shell::alloc_id(next_id);
            let default_profile = cx.global::<AppSettings>().default_profile_command();

            let (launch, profile_name) =
                launch_with_profile(Some(state), default_profile.clone(), cx);

            let pane = match spawn_pane(cx, surface_id, launch, profile_name) {
                Ok(pane) => {
                    Shell::watch_pane(&pane, cx);

                    pane
                }
                Err(error) => {
                    warn!("failed to restore tab {surface_id} lazily: {error}");

                    spawn_default_pane(cx, surface_id, default_profile, None)
                }
            };

            TerminalLayout::new_leaf(PaneId(surface_id), pane)
        });

    *workspaces.active_tabs_mut().active_mut() = TabSurface::Live(tree);

    true
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
        if tab_state.agent.is_none() && tab_state.team_room.is_none() && tab_state.git_cwd.is_none()
        {
            resolve_restored_launch(&mut tab_state, cx.global::<AppSettings>());
        }

        let name = tab_state
            .name
            .clone()
            .filter(|n| !n.trim().is_empty())
            .filter(|n| tab_state.user_named || !legacy_generated_tab_title(n));

        // The profile-derived title a live pane would report, so pending
        // tabs label identically to spawned ones. Unknown agent kinds
        // materialize as terminals, so they take the profile title too.
        let default_title = if tab_state.git_cwd.is_some() {
            t!("git-tab-title").into_owned()
        } else if tab_state.team_room.is_some() {
            t!("team-title").into_owned()
        } else {
            match tab_state.agent.as_deref().and_then(AgentKind::from_id) {
                Some(kind) => {
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
                None => cx
                    .global::<AppSettings>()
                    .profile_name_for_command(tab_state.shell.as_deref(), &tab_state.args),
            }
        };

        restored.push((
            TabSurface::Pending(Box::new(tab_state)),
            TabId(Shell::alloc_id(next_id)),
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
    cx: &mut Context<Shell>,
) -> Option<PaneNode<Entity<TerminalPane>>> {
    match node {
        PaneNodeState::Leaf {
            shell,
            args,
            cwd,
            grid_size,
        } => {
            let surface_id = Shell::alloc_id(next_id);

            let mut launch = TabState {
                git_cwd: None,
                team_room: None,
                name: None,
                user_named: false,
                shell: shell.clone(),
                args: args.clone(),
                cwd: cwd.clone(),
                agent: None,
                agent_profile: None,
                panes: None,
                grid_size: *grid_size,
            };

            resolve_restored_launch(&mut launch, cx.global::<AppSettings>());

            let (launch, profile_name) = launch_with_profile(Some(launch), (None, Vec::new()), cx);

            match spawn_pane(cx, surface_id, launch, profile_name) {
                Ok(pane) => {
                    Shell::watch_pane(&pane, cx);

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

/// Spawn a pane on the default profile, starting the shell in `cwd` when
/// given. Falls back in layers: an unusable cwd retries without it, a
/// broken profile retries the built-in shell.
pub(super) fn spawn_default_pane(
    cx: &mut Context<Shell>,
    surface_id: u64,
    default_profile: (Option<String>, Vec<String>),
    cwd: Option<String>,
) -> Entity<TerminalPane> {
    let launch = cwd.map(|cwd| TabState {
        shell: default_profile.0.clone(),
        args: default_profile.1.clone(),
        cwd: Some(cwd),
        agent: None,
        ..TabState::default()
    });

    let (launch, profile_name) = launch_with_profile(launch, default_profile.clone(), cx);

    let spawned = spawn_pane(cx, surface_id, launch, profile_name).or_else(|error| {
        warn!("spawn with workspace cwd/profile failed, retrying default: {error}");

        let (launch, profile_name) = launch_with_profile(None, default_profile, cx);

        spawn_pane(cx, surface_id, launch, profile_name)
    });

    let pane = match spawned {
        Ok(pane) => pane,
        Err(error) => {
            warn!("default profile failed, retrying built-in shell: {error}");

            let (launch, profile_name) = launch_with_profile(None, (None, Vec::new()), cx);

            match spawn_pane(cx, surface_id, launch, profile_name) {
                Ok(pane) => pane,
                Err(error) => {
                    // Even the built-in shell cannot spawn (e.g. ConPTY
                    // unavailable) — no terminal can ever open, so tell
                    // the user why before exiting instead of dying with
                    // an invisible panic.
                    crate::show_startup_error_dialog(&t!(
                        "startup-terminal-spawn-error",
                        error = error
                    ));

                    process::exit(1);
                }
            }
        }
    };

    Shell::watch_pane(&pane, cx);

    pane
}

/// The window's workspaces and tabs in the shape `local_state` stores.
pub(super) fn session_state(
    workspaces: &WorkspaceManager,
    doomed: Option<WorkspaceId>,
    cx: &App,
) -> SessionState {
    // A doomed workspace (user asked to close the last one) is skipped, so
    // it never reaches local_state; the active index is recomputed over
    // the kept workspaces.
    let mut active_workspace = 0usize;
    let mut saved = Vec::new();

    let default_profile = cx.global::<AppSettings>().default_profile_command();

    for workspace in workspaces.summaries() {
        if Some(workspace.id) == doomed {
            continue;
        }

        // The settings entry is a view of the configuration file, not
        // work to come back to, so it is never restored.
        if workspace.kind == WorkspaceKind::Settings {
            continue;
        }

        // A temporary workspace is scratch space the user has not adopted;
        // its tabs go with it, while tabs opened in an adopted workspace
        // are saved below like any other.
        if workspace.temporary {
            continue;
        }

        let Some(tabs) = workspaces.tabs_of(workspace.id) else {
            continue;
        };

        if workspace.active {
            active_workspace = saved.len();
        }

        saved.push(WorkspaceState {
            name: workspace.name,
            cwd: (!workspace.cwd.is_empty()).then_some(workspace.cwd),
            additional_cwds: workspace.additional_cwds,
            pinned: workspace.pinned,
            active_tab: tabs.list().active_index(),
            tabs: tabs
                .list()
                .items()
                .iter()
                .map(|tab| {
                    let mut state = match tab.surface() {
                        // A tab that never went live re-saves its restored
                        // snapshot unchanged — its shells never ran, so the
                        // saved launch state is still the truth.
                        TabSurface::Pending(state) => (**state).clone(),
                        // Flat fields always mirror the focused pane, so a
                        // snapshot without splits stays in the old format
                        // and an old build restores something sensible
                        // from a split one.
                        TabSurface::Live(tree) => {
                            let mut state = tree.tree().focused_pane().read(cx).tab_state();

                            state.panes = (!tree.tree().is_single_leaf())
                                .then(|| pane_node_state(tree.tree().root(), &default_profile, cx));

                            state
                        }
                        // Agent conversations are not persisted (the
                        // agent process and its thread die with the app);
                        // the saved kind reopens a fresh agent tab.
                        TabSurface::Agent(tab) => {
                            let profile = tab.owner.session().read(cx).profile();
                            let agent: &str = profile.kind.into();

                            TabState {
                                agent: Some(agent.into()),
                                agent_profile: Some(profile.name.clone()),
                                ..TabState::default()
                            }
                        }
                        // Only the settings workspace holds this surface,
                        // and that workspace is skipped above; the arm
                        // exists so the match stays exhaustive.
                        TabSurface::Settings => TabState::default(),
                        TabSurface::Git(tab) => TabState {
                            git_cwd: Some(tab.view.read(cx).cwd().to_string()),
                            ..TabState::default()
                        },
                        TabSurface::Team(pane) => TabState {
                            team_room: Some(pane.read(cx).room_id(cx).to_string()),
                            ..TabState::default()
                        },
                        TabSurface::TeamUnavailable { saved, .. }
                        | TabSurface::TeamDisabled(saved) => (**saved).clone(),
                    };

                    normalize_saved_launch(&mut state, &default_profile);

                    state.name = tab.user_title().map(str::to_owned);
                    state.user_named = state.name.is_some();

                    state
                })
                .collect(),
        });
    }

    SessionState {
        active_workspace,
        workspaces: saved,
    }
}

#[cfg(test)]
mod launch_resolution_tests {
    use gpui::TestAppContext;
    use nmt_config::Config;
    use nmt_config::local_state::TabState;
    use nmt_config::profile::ProfilesConfig;

    use crate::ui::persistence::{
        legacy_generated_tab_title, normalize_saved_launch, resolve_restored_launch, restore_tabs,
        restore_team_tab,
    };
    use crate::ui::settings::{AppSettings, Profile};
    use crate::ui::shell::TabSurface;

    fn settings_with_pwsh_default() -> AppSettings {
        AppSettings::from_config(Config {
            profiles: ProfilesConfig {
                list: vec![
                    Profile {
                        name: "PowerShell".into(),
                        shell: r"C:\Program Files\PowerShell\7\pwsh.exe".into(),
                        args: String::new(),
                    },
                    Profile {
                        name: "WSL".into(),
                        shell: "wsl.exe".into(),
                        args: "-d Ubuntu".into(),
                    },
                ],
                default: "PowerShell".into(),
            },
            ..Config::default()
        })
    }

    fn tab(shell: Option<&str>, args: &[&str]) -> TabState {
        TabState {
            name: None,
            user_named: false,
            shell: shell.map(str::to_string),
            args: args.iter().map(|a| a.to_string()).collect(),
            cwd: None,
            agent: None,
            agent_profile: None,
            team_room: None,
            git_cwd: None,
            panes: None,
            grid_size: None,
        }
    }

    #[gpui::test]
    fn restored_git_tab_keeps_its_directory_without_resolving_a_shell(cx: &mut TestAppContext) {
        cx.update(|cx| {
            cx.set_global(settings_with_pwsh_default());

            let saved = TabState {
                git_cwd: Some("/project".into()),
                ..TabState::default()
            };

            let tabs = restore_tabs(vec![saved.clone()], 0, &mut 0, cx).unwrap();

            assert!(tabs.active().is_git());
            assert!(tabs.active().tree().is_none());
            assert_eq!(
                tabs.list().items()[0].title(),
                rust_i18n::t!("git-tab-title")
            );

            let TabSurface::Pending(restored) = tabs.active() else {
                panic!("restored tab should start pending");
            };

            assert_eq!(restored.as_ref(), &saved);
        });
    }

    #[gpui::test]
    fn restored_team_tabs_keep_their_title(cx: &mut TestAppContext) {
        cx.update(|cx| {
            cx.set_global(settings_with_pwsh_default());

            let saved = TabState {
                team_room: Some("saved-room".into()),
                ..TabState::default()
            };

            let named = TabState {
                name: Some("Review team".into()),
                user_named: true,
                ..saved.clone()
            };

            let tabs = restore_tabs(vec![saved, named], 0, &mut 0, cx).unwrap();

            assert_eq!(tabs.list().items()[0].title(), rust_i18n::t!("team-title"));
            assert_eq!(tabs.list().items()[1].title(), "Review team");
        });
    }

    #[gpui::test]
    fn disabled_agent_team_restore_keeps_saved_state_without_opening_it(cx: &mut TestAppContext) {
        cx.set_global(settings_with_pwsh_default());

        let cx = cx.add_empty_window();

        cx.update(|window, cx| {
            let saved = TabState {
                team_room: Some("invalid-room-id".into()),
                name: Some("Review team".into()),
                user_named: true,
                ..TabState::default()
            };

            for enabled in [false, true, false] {
                cx.global_mut::<AppSettings>()
                    .edit_agent(|agent| agent.enable_agent_team = enabled);

                let surface = restore_team_tab("invalid-room-id", &saved, window, cx);

                let restored = match surface {
                    TabSurface::TeamDisabled(restored) if !enabled => restored,
                    TabSurface::TeamUnavailable { saved, .. } if enabled => saved,
                    _ => panic!("team restore did not honor the current setting"),
                };

                assert_eq!(*restored, saved);
            }
        });
    }

    #[test]
    fn recognizes_generated_titles_from_legacy_snapshots() {
        assert!(legacy_generated_tab_title("Tab 1"));
        assert!(legacy_generated_tab_title("Tab 42"));
        assert!(!legacy_generated_tab_title("Tab"));
        assert!(!legacy_generated_tab_title("editor"));
    }

    #[test]
    fn default_profile_pane_saves_as_follow_default() {
        let default = (Some("pwsh.exe".to_string()), vec!["-NoLogo".to_string()]);

        let mut state = tab(Some("pwsh.exe"), &["-NoLogo"]);

        normalize_saved_launch(&mut state, &default);

        assert_eq!(state.shell, None);
        assert!(state.args.is_empty());
    }

    #[test]
    fn pinned_pane_keeps_its_saved_command() {
        let default = (Some("pwsh.exe".to_string()), Vec::new());

        let mut state = tab(Some("wsl.exe"), &["-d", "Ubuntu"]);

        normalize_saved_launch(&mut state, &default);

        assert_eq!(state.shell.as_deref(), Some("wsl.exe"));
        assert_eq!(state.args, vec!["-d".to_string(), "Ubuntu".to_string()]);
    }

    #[test]
    fn restore_resolves_none_to_default_profile() {
        let settings = settings_with_pwsh_default();

        let mut state = tab(None, &[]);

        resolve_restored_launch(&mut state, &settings);

        assert_eq!(
            state.shell.as_deref(),
            Some(r"C:\Program Files\PowerShell\7\pwsh.exe")
        );
        assert!(state.args.is_empty());
    }

    #[test]
    fn restore_replaces_stale_shell_with_default_profile() {
        // The former built-in default is no longer in the profile list: the saved
        // pin is stale and must follow the current default profile instead.
        let settings = settings_with_pwsh_default();

        let mut state = tab(
            Some(r"C:\WINDOWS\System32\WindowsPowerShell\v1.0\powershell.exe"),
            &[],
        );

        resolve_restored_launch(&mut state, &settings);

        assert_eq!(
            state.shell.as_deref(),
            Some(r"C:\Program Files\PowerShell\7\pwsh.exe")
        );
    }

    #[test]
    fn restore_keeps_shell_still_present_in_profiles() {
        let settings = settings_with_pwsh_default();

        let mut state = tab(Some("WSL.EXE"), &["-d", "Ubuntu"]);

        resolve_restored_launch(&mut state, &settings);

        assert_eq!(state.shell.as_deref(), Some("WSL.EXE"));
        assert_eq!(state.args, vec!["-d".to_string(), "Ubuntu".to_string()]);
    }
}
