//! A window's workspaces and tabs captured in the shape `local_state` stores.

#[cfg(test)]
#[path = "snapshot_tests.rs"]
mod tests;

use app::terminal_tab::view::TerminalPane;
use gpui::{App, Axis, Entity};
use nmt_config::local_state::{
    PaneNodeState, PaneSplitAxis, SessionState, TabState, WorkspaceState,
};

use crate::ui::pane_tree::PaneNode;
use crate::ui::settings::AppSettings;
use crate::ui::shell::TabSurface;
use crate::workspace::{WorkspaceId, WorkspaceKind, WorkspaceManager};

/// A pane that runs the current default profile's exact command is saved with
/// `shell = None` — "follow the default profile" — so later profile changes apply
/// to restored sessions instead of pinning today's shell path forever.
fn normalize_saved_launch(state: &mut TabState, default_profile: &(Option<String>, Vec<String>)) {
    if state.shell == default_profile.0 && state.args == default_profile.1 {
        state.shell = None;
        state.args = Vec::new();
    }
}

fn axis_to_state(axis: Axis) -> PaneSplitAxis {
    match axis {
        Axis::Horizontal => PaneSplitAxis::Horizontal,
        Axis::Vertical => PaneSplitAxis::Vertical,
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

/// The window's workspaces and tabs in the shape `local_state` stores.
pub(crate) fn session_state(
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
