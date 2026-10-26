//! A window's saved session: rebuilding it on launch, and the starting
//! session of a window with none.

pub(super) use crate::ui::persistence::snapshot::session_state;

mod snapshot;

#[cfg(test)]
mod tests;

use std::mem;
use std::time::SystemTime;

use app::agent_tab::execution::AgentSession;
use app::agent_tab::orchestration::OrchestrationPane;
use app::agent_tab::team::{TeamPane, TeamRuntime};
use app::agent_tab::{AgentKind, AgentPane, thread_settings_from_saved};
use app::terminal_tab::view::TerminalPane;
use dirs::home_dir;
use gpui::{App, AppContext, Axis, Context, Entity, Window};
use gpui_component::resizable::ResizableState;
use nmt_agent::chat::SessionSummary;
use nmt_agent::orchestration::run::RunId;
use nmt_agent::team::model::RoomId;
use nmt_config::config_dir_path;
use nmt_config::local_state::{
    PaneNodeState, PaneSplitAxis, SessionState, TabState, WorkspaceState,
};
use rust_i18n::t;
use tracing::warn;

use crate::tabs::{TabId, TabManager};
use crate::ui::AppWindow;
use crate::ui::git_sidebar::GitSidebar;
use crate::ui::pane_tree::{PaneId, PaneNode, PaneTree};
use crate::ui::persistence::snapshot::surface_snapshot;
use crate::ui::remote::{
    new_shared_agent_id, offer_pending_terminal, offer_restoring_agent, paired_host_name,
    restore_agent, restore_view, start_listed_terminal,
};
use crate::ui::settings::{AgentProfile, AppSettings, builtin_agent_profile};
use crate::ui::shell::tab_surface::{AgentTab, GitTab, TerminalPaneTree};
use crate::ui::shell::{TabSurface, agent_workspace};
use crate::ui::terminal_launch::{
    launch_with_profile, spawn_default_pane, spawn_pane, spawn_remote_pane,
};
use crate::ui::terminal_layout::TerminalLayout;
use crate::workspace::{WorkspaceId, WorkspaceManager, WorkspaceRoots, default_workspace_name};

/// What a saved tab reopens as.
enum SavedTab<'a> {
    /// A git review of this directory.
    Git(&'a str),
    /// The team room with this saved id.
    Team(&'a str),
    /// An Orchestration tab, showing the run with this saved id if any.
    Orchestration(Option<&'a str>),
    /// A conversation of this agent kind: the saved one when the tab held
    /// one, otherwise a fresh one.
    Agent(AgentKind),
    /// A terminal session on a paired host, by host id and session id.
    Remote(&'a str, &'a str),
    /// A terminal. An agent kind this build does not know (a newer snapshot)
    /// degrades to a terminal instead of losing the tab.
    Terminal,
}

fn saved_tab(state: &TabState) -> SavedTab<'_> {
    if let Some(cwd) = state.git_cwd.as_deref() {
        return SavedTab::Git(cwd);
    }

    if let Some(room) = state.team_room.as_deref() {
        return SavedTab::Team(room);
    }

    if state.orchestration {
        return SavedTab::Orchestration(state.orchestration_run.as_deref());
    }

    if let (Some(host), Some(session)) = (
        state.remote_host.as_deref(),
        state.remote_session.as_deref(),
    ) {
        return SavedTab::Remote(host, session);
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
/// the built-in profile, so the tab always reopens even after the profile
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
    // The default (no-CLI) branch keeps spawning with no cwd; the shell
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
            tab_fold,
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

        workspaces
            .as_mut()
            .expect("a workspace was just added")
            .set_tab_fold(workspace_id, tab_fold);
    }

    let mut workspaces = workspaces?;

    let active_index = saved_active.min(workspaces.list().len() - 1);

    workspaces.list_mut().activate(active_index);

    // The initially visible tab spawns right away; everything else stays
    // pending, so this window's other `active_pane` readers (activation
    // observers, notification pumps) never see a pending active tab.
    materialize_active_tab(&mut workspaces, next_id, window, cx);

    offer_restoring_agents(&workspaces, window, cx);

    Some(workspaces)
}

/// Offer paired devices the agent tabs still waiting to be restored, under
/// the ids they knew them by, so their views survive this restart.
fn offer_restoring_agents(
    workspaces: &WorkspaceManager,
    window: &mut Window,
    cx: &mut Context<AppWindow>,
) {
    let offers: Vec<_> = workspaces
        .all_tabs()
        .flat_map(|tabs| tabs.list().items())
        .filter_map(|tab| {
            let TabSurface::Pending(state) = tab.surface() else {
                return None;
            };

            let SavedTab::Agent(kind) = saved_tab(state) else {
                return None;
            };

            let harness: &str = kind.into();

            Some((
                state.shared_agent.clone()?,
                tab.title().to_owned(),
                harness.to_owned(),
            ))
        })
        .collect();

    for (id, title, harness) in offers {
        offer_pending_agent(id, title, harness, window, cx);
    }
}

/// Offer paired devices one pending agent tab of this window under `id`;
/// the first device request starts it in place.
fn offer_pending_agent(
    id: String,
    title: String,
    harness: String,
    window: &mut Window,
    cx: &mut Context<AppWindow>,
) {
    let this = cx.entity().downgrade();
    let handle = window.window_handle();
    let restored = id.clone();

    let restore = move |cx: &mut App| {
        handle
            .update(cx, |_, window, cx| {
                this.update(cx, |this, cx| this.start_pending_tab(&restored, window, cx))
            })
            .is_ok_and(|restored| restored.unwrap_or(false))
    };

    offer_restoring_agent(id, title, harness, restore, cx);
}

/// Put the tab `id` to sleep the way a restored tab starts: its surface
/// becomes a hibernated snapshot, and paired
/// devices keep listing it as a tab waiting to start. Returns the surface it
/// replaced, whose panes and agent session end once the caller drops it, or
/// `None` when the tab runs nothing a snapshot could restart.
pub(super) fn hibernate_tab(
    workspaces: &mut WorkspaceManager,
    id: TabId,
    window: &mut Window,
    cx: &mut Context<AppWindow>,
) -> Option<TabSurface> {
    let tab = workspaces
        .workspace_of_tab(id)
        .and_then(|workspace| workspaces.tabs_of(workspace))
        .and_then(|tabs| tabs.list().find(id))
        .filter(|tab| tab.surface().can_hibernate(cx))?;

    let title = tab.title().to_owned();
    let default_profile = cx.global::<AppSettings>().default_profile_command();

    let mut state = surface_snapshot(tab.surface(), &default_profile, cx);

    // The live surface still holds its device listing until it is dropped,
    // and dropping it withdraws that id later in this update, so the sleeping
    // tab takes a fresh id instead of one about to be withdrawn.
    match saved_tab(&state) {
        SavedTab::Agent(kind) => {
            let shared = new_shared_agent_id();
            let harness: &str = kind.into();

            offer_pending_agent(shared.clone(), title, harness.to_owned(), window, cx);

            state.shared_agent = Some(shared);
        }
        SavedTab::Terminal => state.shared_terminal = offer_pending_terminal(title, cx),
        SavedTab::Git(_)
        | SavedTab::Team(_)
        | SavedTab::Orchestration(_)
        | SavedTab::Remote(..) => {}
    }

    let tab = workspaces
        .tabs_for_tab_mut(id)
        .and_then(|tabs| tabs.list_mut().find_mut(id))?;

    Some(mem::replace(
        tab.surface_mut(),
        TabSurface::Hibernated(Box::new(state)),
    ))
}

/// Spawn the shells of a still-pending active tab and swap its surface to
/// `Live`. Returns whether a materialization happened. A saved pane layout
/// rebuilds the split tree (one fresh shell per leaf); an unusable layout
/// or failed spawn degrades to a single default-profile pane so the tab
/// the user just activated never vanishes. A tab saved with an agent kind
/// reopens as an agent tab instead, continuing its saved conversation once
/// the new session is ready (the harness process died with the previous app
/// instance, but the conversation itself persists on disk).
pub(super) fn materialize_active_tab(
    workspaces: &mut WorkspaceManager,
    next_id: &mut u64,
    window: &mut Window,
    cx: &mut Context<AppWindow>,
) -> bool {
    let state = match workspaces.active_tabs().active() {
        TabSurface::Pending(state) | TabSurface::Hibernated(state) => (**state).clone(),
        TabSurface::TeamDisabled(state)
            if cx.global::<AppSettings>().config().agent.enable_agent_team =>
        {
            (**state).clone()
        }
        TabSurface::OrchestrationDisabled(state)
            if cx
                .global::<AppSettings>()
                .config()
                .agent
                .enable_agent_orchestration =>
        {
            (**state).clone()
        }
        TabSurface::Live(_)
        | TabSurface::Agent(_)
        | TabSurface::Git(_)
        | TabSurface::Settings
        | TabSurface::Team(_)
        | TabSurface::TeamDisabled(_)
        | TabSurface::TeamUnavailable { .. }
        | TabSurface::Orchestration(_)
        | TabSurface::OrchestrationDisabled(_) => return false,
    };

    let roots = workspaces.active_roots().cloned();
    let surface = restored_surface(state, roots.as_ref(), next_id, window, cx);

    *workspaces.active_tabs_mut().active_mut() = surface;

    true
}

/// Start the pending tab `id` in place, in whichever workspace holds it,
/// without activating it. Returns whether a pending tab was started.
pub(super) fn materialize_tab(
    workspaces: &mut WorkspaceManager,
    id: TabId,
    next_id: &mut u64,
    window: &mut Window,
    cx: &mut Context<AppWindow>,
) -> bool {
    let Some(workspace) = workspaces.workspace_of_tab(id) else {
        return false;
    };

    let state = match workspaces
        .tabs_of(workspace)
        .and_then(|tabs| tabs.list().find(id))
        .map(|tab| tab.surface())
    {
        Some(TabSurface::Pending(state) | TabSurface::Hibernated(state)) => (**state).clone(),
        _ => return false,
    };

    let roots = workspaces.roots_of(workspace).cloned();
    let surface = restored_surface(state, roots.as_ref(), next_id, window, cx);

    if let Some(tab) = workspaces
        .tabs_for_tab_mut(id)
        .and_then(|tabs| tabs.list_mut().find_mut(id))
    {
        *tab.surface_mut() = surface;
    }

    true
}

/// The live surface a saved tab reopens as, in a workspace with `roots`.
fn restored_surface(
    state: TabState,
    roots: Option<&WorkspaceRoots>,
    next_id: &mut u64,
    window: &mut Window,
    cx: &mut Context<AppWindow>,
) -> TabSurface {
    match saved_tab(&state) {
        SavedTab::Git(cwd) => {
            let view = cx.new(|cx| GitSidebar::new(cwd.to_owned(), window, cx));

            TabSurface::Git(GitTab {
                view,
                return_to: None,
            })
        }
        SavedTab::Team(saved_id) => restore_team_tab(saved_id, &state, window, cx),
        SavedTab::Orchestration(run) => restore_orchestration_tab(run, &state, roots, window, cx),
        SavedTab::Agent(kind) => {
            let workspace = agent_workspace(roots);

            let profile = restored_agent_profile(
                state.agent_profile.as_deref(),
                kind,
                cx.global::<AppSettings>(),
            );

            let owner = AgentSession::create(profile, workspace, None, cx);

            // The tab reopens on the controls it was left set to; a tab the
            // user never adjusted saved none and starts from its profile.
            if let Some(saved) = state.agent_settings.as_ref() {
                let settings = thread_settings_from_saved(saved);

                owner
                    .session()
                    .clone()
                    .update(cx, |session, _| session.remember_settings(settings));
            }

            // A restored tab has no history-list row to name it, so the title
            // stays empty and the resume takes the name the transcript
            // records; only the id and title take part in a resume.
            if let Some(id) = state.agent_conversation.clone() {
                let summary = SessionSummary {
                    id,
                    title: String::new(),
                    branch: None,
                    cwd: None,
                    last_active: SystemTime::UNIX_EPOCH,
                    snippet: None,
                    origin: None,
                };

                owner
                    .session()
                    .clone()
                    .update(cx, |session, _| session.resume_when_ready(summary));
            }

            let pane = cx.new(|cx| AgentPane::attach(&owner, window, cx));

            AppWindow::watch_agent_tab(&pane, state.shared_agent.clone(), cx);

            owner.start(None, cx);

            TabSurface::Agent(AgentTab { owner, pane })
        }
        SavedTab::Remote(host, session) => {
            match state.agent.as_deref().and_then(AgentKind::from_id) {
                Some(kind) => match restore_agent(host, session, kind, window, cx) {
                    Some((owner, pane)) => {
                        AppWindow::watch_agent_tab(&pane, None, cx);

                        TabSurface::Agent(AgentTab { owner, pane })
                    }
                    None => {
                        TabSurface::Live(restore_terminal_tree(TabState::default(), next_id, cx))
                    }
                },
                None => match restore_remote_pane(host, session, next_id, cx) {
                    Some(tree) => TabSurface::Live(tree),
                    None => {
                        TabSurface::Live(restore_terminal_tree(TabState::default(), next_id, cx))
                    }
                },
            }
        }
        SavedTab::Terminal => {
            let listed = state.shared_terminal.clone();

            TabSurface::Live(start_listed_terminal(listed, cx, |cx| {
                restore_terminal_tree(state, next_id, cx)
            }))
        }
    }
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

/// A saved remote tab, reattached to its session. The session kept running
/// on the host; a host that is unreachable now shows its reconnecting
/// state in the tab. `None` when the host is no longer paired.
fn restore_remote_pane(
    host: &str,
    session: &str,
    next_id: &mut u64,
    cx: &mut Context<AppWindow>,
) -> Option<TerminalPaneTree> {
    let pty = restore_view(host, session, cx)?;
    let surface_id = AppWindow::alloc_id(next_id);

    // A restored view does not end the session when closed: whether this
    // client started it is not recorded.
    let pane = spawn_remote_pane(cx, surface_id, pty, false)
        .inspect_err(|error| warn!("failed to restore a remote tab: {error}"))
        .ok()?;

    AppWindow::watch_pane(&pane, cx);

    Some(TerminalLayout::new_leaf(PaneId(surface_id), pane))
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
        .map(|id| TeamRuntime::open(&config_dir_path(), id, cx));

    match restored {
        Ok(runtime) => TabSurface::Team(cx.new(|cx| TeamPane::new(runtime, window, cx))),
        Err(message) => TabSurface::TeamUnavailable {
            saved: Box::new(state.clone()),
            message,
        },
    }
}

fn restore_orchestration_tab(
    run: Option<&str>,
    state: &TabState,
    roots: Option<&WorkspaceRoots>,
    window: &mut Window,
    cx: &mut App,
) -> TabSurface {
    if !cx
        .global::<AppSettings>()
        .config()
        .agent
        .enable_agent_orchestration
    {
        return TabSurface::OrchestrationDisabled(Box::new(state.clone()));
    }

    let run = run.and_then(|id| id.parse::<RunId>().ok());
    let workspace = agent_workspace(roots);

    TabSurface::Orchestration(
        cx.new(|cx| OrchestrationPane::new(config_dir_path(), workspace, run, window, cx)),
    )
}

/// Rebuild a workspace's tabs as pending surfaces: the saved snapshot is
/// kept per tab and no shell spawns here; `materialize_active_tab` turns
/// a tab live the first time it is activated.
fn restore_tabs(
    tabs: Vec<TabState>,
    active_tab: usize,
    next_id: &mut u64,
    cx: &App,
) -> Option<TabManager<TabSurface>> {
    let mut restored = Vec::new();

    for mut tab_state in tabs {
        match saved_tab(&tab_state) {
            // Agent tabs carry no launch command; profile resolution only
            // applies to terminal tabs.
            SavedTab::Terminal => {
                resolve_restored_launch(&mut tab_state, cx.global::<AppSettings>());
            }
            // Earlier builds saved tabs following a paired host's session
            // among the local ones. Such a tab runs on the host, and the
            // host's own list reopens it, so it does not come back here as
            // a tab that passes for a local one.
            SavedTab::Remote(..) => continue,
            // A pending tab re-saves its snapshot unchanged, so an agent tab
            // saved before it was ever offered would stay unlisted on paired
            // devices until someone here activates it. A fresh id offers it
            // now, and the tab keeps that id once it goes live.
            SavedTab::Agent(_) => {
                tab_state
                    .shared_agent
                    .get_or_insert_with(new_shared_agent_id);
            }
            SavedTab::Git(_) | SavedTab::Team(_) | SavedTab::Orchestration(_) => {}
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
            SavedTab::Orchestration(_) => t!("orchestration-title").into_owned(),
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
            SavedTab::Remote(host, _) => {
                paired_host_name(host, cx).unwrap_or_else(|| t!("remote-tab-title").into_owned())
            }
            SavedTab::Terminal => cx
                .global::<AppSettings>()
                .profile_name_for_command(tab_state.shell.as_deref(), &tab_state.args),
        };

        // The title the tab's content last reported labels it until it
        // spawns and reports its own.
        let title = tab_state.title.clone();

        // Paired devices list a terminal tab while its shell waits to start,
        // under the title a running one would report.
        if let SavedTab::Terminal = saved_tab(&tab_state) {
            let listed = title.clone().unwrap_or_else(|| default_title.clone());

            tab_state.shared_terminal = offer_pending_terminal(listed, cx);
        }

        restored.push((
            TabSurface::Pending(Box::new(tab_state)),
            TabId(AppWindow::alloc_id(next_id)),
            name,
            title,
            default_title,
        ));
    }

    let mut restored = restored.into_iter();

    let (first_pane, first_id, first_name, first_title, first_default_title) = restored.next()?;

    let mut tab_manager = TabManager::new(first_pane, first_id, first_default_title);

    restore_titles(&mut tab_manager, first_id, first_name, first_title);

    for (pane, id, name, title, default_title) in restored {
        tab_manager.new_tab(pane, id, default_title);

        restore_titles(&mut tab_manager, id, name, title);
    }

    let active_index = active_tab.min(tab_manager.list().len() - 1);

    tab_manager.list_mut().activate(active_index);

    Some(tab_manager)
}

fn restore_titles(
    tab_manager: &mut TabManager<TabSurface>,
    id: TabId,
    name: Option<String>,
    title: Option<String>,
) {
    if let Some(title) = title {
        tab_manager.set_title(id, title);
    }

    if let Some(name) = name {
        tab_manager.rename(id, name);
    }
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
