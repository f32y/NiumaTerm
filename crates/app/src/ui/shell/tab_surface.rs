use std::path::Path;

use app::agent_tab::execution::{AgentSession, SessionOwner};
use app::agent_tab::orchestration::OrchestrationPane;
use app::agent_tab::team::TeamPane;
use app::agent_tab::{AgentKind, AgentPane};
use app::terminal_tab::view::TerminalPane;
use gpui::{App, Entity, EntityId};
use gpui_component::{Icon, IconName, Sizable as _};
use nmt_agent::AgentRoute;
use nmt_config::local_state::{PaneNodeState, TabState};
use nmt_platform::filesystem::path_identity;
use tracing::warn;

use crate::tabs::TabId;
use crate::ui::git_sidebar::GitSidebar;
use crate::ui::pane_tree::PaneId;
use crate::ui::tab_bar::menu::tab_icon;
use crate::ui::terminal_layout::TerminalLayout;

pub(crate) type TerminalPaneTree = TerminalLayout<Entity<TerminalPane>>;

pub(crate) struct AgentTab {
    pub(crate) owner: SessionOwner,
    pub(crate) pane: Entity<AgentPane>,
}

pub(crate) struct GitTab {
    pub(crate) view: Entity<GitSidebar>,
    pub(crate) return_to: Option<TabId>,
}

/// A tab's surface. Restored tabs start `Pending` (the saved snapshot with no
/// shell process behind it) and become `Live` (spawning their shells) the
/// first time they are activated, so startup only pays for the visible tab.
/// Hibernated tabs retain their saved state until the user wakes them.
pub(crate) enum TabSurface {
    Pending(Box<TabState>),
    /// A saved surface that starts only after the user chooses to wake it.
    Hibernated(Box<TabState>),
    Live(TerminalPaneTree),
    /// An agent conversation rendered as chat bubbles instead of a terminal
    /// grid. It owns an agent route but no terminal panes or child-process
    /// accounting exposed through `tree()`.
    Agent(AgentTab),
    /// The settings UI filling the main area. It is rebuilt from the settings
    /// global on every render, so the variant holds no state of its own.
    Settings,
    Git(GitTab),
    Team(Entity<TeamPane>),
    TeamUnavailable {
        saved: Box<TabState>,
        message: String,
    },
    TeamDisabled(Box<TabState>),
    Orchestration(Entity<OrchestrationPane>),
    OrchestrationDisabled(Box<TabState>),
}

impl TabSurface {
    /// The matching terminal leaf in display order. Saved leaves use the same
    /// order as restored panes, so activation can focus the selected directory.
    pub(super) fn terminal_in_directory(&self, target: &[String], cx: &App) -> Option<usize> {
        let matches = |cwd: &str| path_identity(Path::new(cwd)) == target;

        match self {
            Self::Live(tree) => tree.tree().leaves().iter().position(|(_, pane)| {
                pane.read(cx)
                    .tab_state()
                    .cwd
                    .as_deref()
                    .is_some_and(matches)
            }),
            Self::Pending(state) | Self::Hibernated(state)
                if state.git_cwd.is_none()
                    && state.team_room.is_none()
                    && !state.orchestration
                    && state
                        .agent
                        .as_deref()
                        .and_then(AgentKind::from_id)
                        .is_none() =>
            {
                if let Some(panes) = &state.panes {
                    let mut directories = Vec::new();

                    saved_terminal_directories(panes, &mut directories);

                    directories.iter().position(|cwd| cwd.is_some_and(matches))
                } else {
                    state.cwd.as_deref().is_some_and(matches).then_some(0)
                }
            }
            _ => None,
        }
    }

    /// Every agent route this surface holds: one per terminal pane, and the
    /// Agent session's when it has one.
    pub(crate) fn agent_routes(&self, cx: &App) -> Vec<AgentRoute> {
        let mut routes: Vec<_> = self
            .leaves()
            .into_iter()
            .map(|(_, pane)| pane.read(cx).agent_route().clone())
            .collect();

        if let Some(session) = self.agent_session() {
            routes.push(session.read(cx).agent_route().clone());
        }

        routes
    }

    pub(crate) fn icon(&self, cx: &App) -> Icon {
        match self {
            Self::Git(_) => Icon::new(IconName::GitBranch).xsmall(),
            Self::Pending(state) if state.git_cwd.is_some() => {
                Icon::new(IconName::GitBranch).xsmall()
            }
            Self::Team(_) | Self::TeamUnavailable { .. } | Self::TeamDisabled(_) => {
                Icon::new(IconName::Network).xsmall()
            }
            Self::Pending(state) if state.team_room.is_some() => {
                Icon::new(IconName::Network).xsmall()
            }
            Self::Orchestration(_) | Self::OrchestrationDisabled(_) => {
                Icon::new(IconName::LayoutDashboard).xsmall()
            }
            Self::Pending(state) if state.orchestration => {
                Icon::new(IconName::LayoutDashboard).xsmall()
            }
            _ => tab_icon(self.agent_kind(cx), self.is_settings()),
        }
    }

    pub(crate) fn team(&self) -> Option<&Entity<TeamPane>> {
        match self {
            Self::Team(pane) => Some(pane),
            _ => None,
        }
    }

    pub(crate) fn orchestration(&self) -> Option<&Entity<OrchestrationPane>> {
        match self {
            Self::Orchestration(pane) => Some(pane),
            _ => None,
        }
    }

    /// Close an Orchestration tab's run and keep only what reopens it, for
    /// when the feature is turned off. The run stays saved.
    pub(super) fn disable_orchestration(&mut self, cx: &mut App) -> bool {
        let Self::Orchestration(pane) = self else {
            return false;
        };

        let saved = TabState {
            orchestration: true,
            orchestration_run: pane.read(cx).run_id(cx).map(|id| id.to_string()),
            ..TabState::default()
        };

        pane.update(cx, |pane, cx| pane.close(cx));

        *self = Self::OrchestrationDisabled(Box::new(saved));

        true
    }

    pub(super) fn disable_team(&mut self, cx: &mut App) -> bool {
        let Self::Team(pane) = self else {
            return false;
        };

        let saved = TabState {
            team_room: Some(pane.read(cx).room_id(cx).to_string()),
            ..TabState::default()
        };

        let runtime = pane.read(cx).runtime().clone();

        let closed = runtime.update(cx, |runtime, cx| runtime.close(cx));

        cx.spawn(async move |_| {
            if let Err(error) = closed.await {
                warn!("could not save disabled Team: {error}");
            }

            drop(runtime);
        })
        .detach();

        *self = Self::TeamDisabled(Box::new(saved));

        true
    }

    pub(crate) fn agent_kind(&self, cx: &App) -> Option<AgentKind> {
        match self {
            Self::Agent(tab) => Some(tab.owner.session().read(cx).profile().kind),
            Self::Pending(state) | Self::Hibernated(state) => {
                state.agent.as_deref().and_then(AgentKind::from_id)
            }
            Self::Live(_)
            | Self::Git(_)
            | Self::Settings
            | Self::Team(_)
            | Self::TeamUnavailable { .. }
            | Self::TeamDisabled(_)
            | Self::Orchestration(_)
            | Self::OrchestrationDisabled(_) => None,
        }
    }

    pub(super) fn is_agent(&self) -> bool {
        match self {
            Self::Agent(_) => true,
            Self::Pending(state) | Self::Hibernated(state) => state
                .agent
                .as_deref()
                .and_then(AgentKind::from_id)
                .is_some(),
            Self::Live(_)
            | Self::Git(_)
            | Self::Settings
            | Self::Team(_)
            | Self::TeamUnavailable { .. }
            | Self::TeamDisabled(_)
            | Self::Orchestration(_)
            | Self::OrchestrationDisabled(_) => false,
        }
    }

    /// Whether this tab follows `session` on the paired host `host`.
    pub(crate) fn follows_remote(&self, host: &str, session: &str, cx: &App) -> bool {
        let terminal = self.leaves().into_iter().any(|(_, pane)| {
            pane.read(cx).remote_tab().is_some_and(|remote| {
                remote.host.id().as_str() == host && remote.session == session
            })
        });

        let agent = self.agent().is_some_and(|pane| {
            pane.read(cx)
                .remote_address()
                .is_some_and(|(followed_host, followed)| {
                    followed_host == host && followed == session
                })
        });

        terminal || agent
    }

    /// Whether hibernating can end what this surface runs and its snapshot
    /// restart it later: shells started on this computer, or an agent
    /// running here. A pane following a paired host's session, or viewing a
    /// terminal a device started here, does not own its process, and a fresh
    /// local shell would not bring that session back.
    pub(crate) fn can_hibernate(&self, cx: &App) -> bool {
        match self {
            Self::Live(tree) => tree.tree().leaves().iter().all(|(_, pane)| {
                let pane = pane.read(cx);

                !pane.is_remote() && pane.remote_created_session().is_none()
            }),
            Self::Agent(tab) => tab.pane.read(cx).remote_address().is_none(),
            Self::Pending(_)
            | Self::Hibernated(_)
            | Self::Git(_)
            | Self::Settings
            | Self::Team(_)
            | Self::TeamUnavailable { .. }
            | Self::TeamDisabled(_)
            | Self::Orchestration(_)
            | Self::OrchestrationDisabled(_) => false,
        }
    }

    /// The id paired devices know a still-pending agent or terminal tab by.
    pub(crate) fn pending_session(&self) -> Option<&str> {
        match self {
            Self::Pending(state) | Self::Hibernated(state) => state
                .shared_agent
                .as_deref()
                .or(state.shared_terminal.as_deref()),
            _ => None,
        }
    }

    pub(crate) fn git(&self) -> Option<&GitTab> {
        match self {
            Self::Git(tab) => Some(tab),
            _ => None,
        }
    }

    pub(crate) fn is_git(&self) -> bool {
        matches!(self, Self::Git(_))
            || matches!(self, Self::Pending(state) if state.git_cwd.is_some())
    }

    pub(crate) fn is_settings(&self) -> bool {
        matches!(self, Self::Settings)
    }

    /// The live pane tree for callers that have already checked the surface.
    pub(crate) fn live(&self) -> &TerminalPaneTree {
        match self {
            TabSurface::Live(tree) => tree,
            _ => unreachable!("tab surface has no live terminal"),
        }
    }

    pub(crate) fn live_mut(&mut self) -> &mut TerminalPaneTree {
        match self {
            TabSurface::Live(tree) => tree,
            _ => unreachable!("tab surface has no live terminal"),
        }
    }

    pub(crate) fn tree(&self) -> Option<&TerminalPaneTree> {
        match self {
            TabSurface::Live(tree) => Some(tree),
            _ => None,
        }
    }

    pub(super) fn tree_mut(&mut self) -> Option<&mut TerminalPaneTree> {
        match self {
            TabSurface::Live(tree) => Some(tree),
            _ => None,
        }
    }

    /// Entity ids of the panes this tab shows, terminal and agent alike.
    pub(crate) fn pane_ids(&self) -> Vec<EntityId> {
        self.leaves()
            .into_iter()
            .map(|(_, pane)| pane.entity_id())
            .chain(self.agent().map(Entity::entity_id))
            .collect()
    }

    pub(super) fn agent(&self) -> Option<&Entity<AgentPane>> {
        match self {
            TabSurface::Agent(tab) => Some(&tab.pane),
            _ => None,
        }
    }

    pub(super) fn agent_session(&self) -> Option<&Entity<AgentSession>> {
        match self {
            Self::Agent(tab) => Some(tab.owner.session()),
            _ => None,
        }
    }

    /// Live leaves. A pending tab has none: it owns no panes and no
    /// processes, which is exactly what route/process sweeps should see.
    pub(crate) fn leaves(&self) -> Vec<(PaneId, &Entity<TerminalPane>)> {
        self.tree()
            .map(|tree| tree.tree().leaves())
            .unwrap_or_default()
    }

    pub(crate) fn contains(&self, id: PaneId) -> bool {
        self.tree().is_some_and(|tree| tree.tree().contains(id))
    }
}

fn saved_terminal_directories<'a>(node: &'a PaneNodeState, directories: &mut Vec<Option<&'a str>>) {
    match node {
        PaneNodeState::Leaf { cwd, .. } => directories.push(cwd.as_deref()),
        PaneNodeState::Split { children, .. } => {
            for child in children {
                saved_terminal_directories(child, directories);
            }
        }
    }
}
