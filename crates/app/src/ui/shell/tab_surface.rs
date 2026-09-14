use app::agent_tab::execution::{AgentSession, SessionOwner};
use app::agent_tab::team::TeamPane;
use app::agent_tab::{AgentKind, AgentPane};
use app::terminal_tab::view::TerminalPane;
use gpui::{App, Entity};
use gpui_component::{Icon, IconName, Sizable as _};
use nmt_config::local_state::TabState;
use tracing::warn;

use crate::pane_tree::PaneId;
use crate::ui::tab_bar::menu::tab_icon;
use crate::ui::terminal_layout::TerminalLayout;

pub(crate) type TerminalPaneTree = TerminalLayout<Entity<TerminalPane>>;

pub(crate) struct AgentTab {
    pub(crate) owner: SessionOwner,
    pub(crate) pane: Entity<AgentPane>,
}

/// A tab's surface. Restored tabs start `Pending` — the saved snapshot with no
/// shell process behind it — and become `Live` (spawning their shells) the
/// first time they are activated, so startup only pays for the visible tab.
pub(crate) enum TabSurface {
    Pending(Box<TabState>),
    Live(TerminalPaneTree),
    /// An agent conversation rendered as chat bubbles instead of a terminal
    /// grid. It owns an agent route but no terminal panes or child-process
    /// accounting exposed through `tree()`.
    Agent(AgentTab),
    /// The settings UI filling the main area. It is rebuilt from the settings
    /// global on every render, so the variant carries no state of its own.
    Settings,
    Team(Entity<TeamPane>),
    TeamUnavailable {
        saved: Box<TabState>,
        message: String,
    },
    TeamDisabled(Box<TabState>),
}

impl TabSurface {
    pub(crate) fn icon(&self, cx: &App) -> Icon {
        match self {
            Self::Team(_) | Self::TeamUnavailable { .. } | Self::TeamDisabled(_) => {
                Icon::new(IconName::Network).xsmall()
            }
            Self::Pending(state) if state.team_room.is_some() => {
                Icon::new(IconName::Network).xsmall()
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

    pub(super) fn disable_team(&mut self, cx: &mut App) -> bool {
        let Self::Team(pane) = self else {
            return false;
        };

        let saved = TabState {
            team_room: Some(pane.read(cx).room_id(cx).to_string()),
            ..TabState::default()
        };

        let runtime = pane.read(cx).runtime().clone();

        if let Err(error) = runtime.update(cx, |runtime, cx| runtime.close(cx)) {
            warn!("could not save disabled Team: {error}");
        }

        *self = Self::TeamDisabled(Box::new(saved));

        true
    }

    pub(crate) fn agent_kind(&self, cx: &App) -> Option<AgentKind> {
        match self {
            Self::Agent(tab) => Some(tab.owner.session().read(cx).profile().kind),
            Self::Pending(state) => state.agent.as_deref().and_then(AgentKind::from_id),
            Self::Live(_)
            | Self::Settings
            | Self::Team(_)
            | Self::TeamUnavailable { .. }
            | Self::TeamDisabled(_) => None,
        }
    }

    pub(super) fn is_agent(&self) -> bool {
        match self {
            Self::Agent(_) => true,
            Self::Pending(state) => state
                .agent
                .as_deref()
                .and_then(AgentKind::from_id)
                .is_some(),
            Self::Live(_)
            | Self::Settings
            | Self::Team(_)
            | Self::TeamUnavailable { .. }
            | Self::TeamDisabled(_) => false,
        }
    }

    pub(crate) fn is_settings(&self) -> bool {
        matches!(self, Self::Settings)
    }

    /// The live pane tree. Every activation path materializes the newly active
    /// tab before touching its surface, so active-tab code may assume `Live`.
    pub(crate) fn live(&self) -> &TerminalPaneTree {
        match self {
            TabSurface::Live(tree) => tree,
            _ => unreachable!("active tab surface is always live"),
        }
    }

    pub(crate) fn live_mut(&mut self) -> &mut TerminalPaneTree {
        match self {
            TabSurface::Live(tree) => tree,
            _ => unreachable!("active tab surface is always live"),
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

    /// Live leaves. A pending tab has none — it owns no panes and no
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
