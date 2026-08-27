use gpui::{App, Entity};
use gpui_component::resizable::ResizableState;
use nmt_app_agent::{AgentKind, AgentPane};
use nmt_app_terminal::view::TerminalPane;
use nmt_config::local_state::TabState;

use crate::pane_tree::{PaneId, PaneTree};

pub(crate) type TerminalPaneTree = PaneTree<Entity<TerminalPane>, Entity<ResizableState>>;

/// A tab's surface. Restored tabs start `Pending` — the saved snapshot with no
/// shell process behind it — and become `Live` (spawning their shells) the
/// first time they are activated, so startup only pays for the visible tab.
pub(crate) enum TabSurface {
    Pending(Box<TabState>),
    Live(TerminalPaneTree),
    /// An agent conversation rendered as chat bubbles instead of a terminal
    /// grid. It owns an agent route but no terminal panes or child-process
    /// accounting exposed through `tree()`.
    Agent(Entity<AgentPane>),
    /// The settings UI filling the main area. It is rebuilt from the settings
    /// global on every render, so the variant carries no state of its own.
    Settings,
}

impl TabSurface {
    pub(crate) fn agent_kind(&self, cx: &App) -> Option<AgentKind> {
        match self {
            Self::Agent(pane) => Some(pane.read(cx).kind()),
            Self::Pending(state) => state.agent.as_deref().and_then(AgentKind::from_id),
            Self::Live(_) | Self::Settings => None,
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
            Self::Live(_) | Self::Settings => false,
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
            TabSurface::Agent(pane) => Some(pane),
            _ => None,
        }
    }

    /// Live leaves. A pending tab has none — it owns no panes and no
    /// processes, which is exactly what route/process sweeps should see.
    pub(crate) fn leaves(&self) -> Vec<(PaneId, &Entity<TerminalPane>)> {
        self.tree().map(|tree| tree.leaves()).unwrap_or_default()
    }

    pub(crate) fn contains(&self, id: PaneId) -> bool {
        self.tree().is_some_and(|tree| tree.contains(id))
    }
}
