use dirs::home_dir;
use gpui::{App, AppContext as _, Axis, Context, Entity};
use gpui_component::resizable::ResizableState;
use nmt_config::local_state::{
    PaneNodeState, PaneSplitAxis, SessionState, TabState, WorkspaceState,
};
use tracing::warn;

use super::Shell;
use super::settings::AppSettings;
use super::shell::TerminalPaneTree;
use crate::pane_tree::{PaneId, PaneNode, PaneTree};
use crate::tabs::{TabId, TabManager};
use crate::terminal::view::TerminalPane;
use crate::window::WindowRegistry;
use crate::workspace::{DEFAULT_WORKSPACE_NAME, WorkspaceId, WorkspaceManager};

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

/// Resolve a saved launch command for restore: a `None` shell follows the current
/// default profile, and a shell matching no configured profile is a stale pin
/// (e.g. a former built-in default the user has since moved away from) that falls
/// back to the default profile as well. Only a shell still present in the profile
/// list keeps its saved command.
fn resolve_restored_launch(state: &mut TabState, settings: &AppSettings) {
    let keep = state.shell.as_deref().is_some_and(|shell| {
        settings
            .profiles
            .iter()
            .any(|p| p.shell.trim().eq_ignore_ascii_case(shell))
    });
    if !keep {
        let (shell, args) = settings.default_profile_command();
        state.shell = shell;
        state.args = args;
    }
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
    node: &PaneNode<Entity<TerminalPane>, Entity<ResizableState>>,
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

impl Shell {
    /// The starting workspace set for a window without a restored session:
    /// one workspace, one tab. With `initial_cwd` (a CLI `new_window` target)
    /// the workspace is rooted there; otherwise it uses the home directory.
    pub(super) fn default_session(
        initial_cwd: Option<String>,
        default_profile: (Option<String>, Vec<String>),
        next_id: &mut u64,
        cx: &mut Context<Self>,
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

        let surface_id = Self::alloc_id(next_id);
        let pane = Self::spawn_default_pane(cx, surface_id, default_profile, spawn_cwd);
        let title = pane.read(cx).profile_name().to_string();
        let tabs = TabManager::new(
            PaneTree::new_leaf(PaneId(surface_id), pane),
            TabId(surface_id),
            title,
        );
        let workspace_id = Self::alloc_id(next_id);

        WorkspaceManager::new(
            tabs,
            WorkspaceId(workspace_id),
            DEFAULT_WORKSPACE_NAME.into(),
            cwd,
            false,
        )
    }

    pub(super) fn restore_session(
        session: Option<SessionState>,
        default_profile: (Option<String>, Vec<String>),
        next_id: &mut u64,
        cx: &mut Context<Self>,
    ) -> Option<WorkspaceManager> {
        let session = session?;
        let saved_active = session.active_workspace;

        let mut workspaces: Option<WorkspaceManager> = None;
        let mut restored_count = 0usize;

        for workspace in session.workspaces {
            let WorkspaceState {
                name,
                cwd,
                pinned,
                active_tab,
                tabs,
            } = workspace;

            let Some(tab_manager) =
                Self::restore_tabs(tabs, active_tab, default_profile.clone(), next_id, cx)
            else {
                continue;
            };

            restored_count += 1;

            let workspace_id = WorkspaceId(Self::alloc_id(next_id));
            let name = if name.trim().is_empty() {
                format!("Workspace {restored_count}")
            } else {
                name
            };
            let cwd = cwd
                .filter(|cwd| !cwd.trim().is_empty())
                .unwrap_or_else(|| ".".to_string());

            if let Some(manager) = &mut workspaces {
                manager.new_workspace_with_pinned(
                    tab_manager,
                    workspace_id,
                    name,
                    cwd,
                    false,
                    pinned,
                );
            } else {
                workspaces = Some(WorkspaceManager::new(
                    tab_manager,
                    workspace_id,
                    name,
                    cwd,
                    false,
                ));
                workspaces
                    .as_mut()
                    .expect("workspace manager was just created")
                    .set_pinned(workspace_id, pinned);
            }
        }

        let mut workspaces = workspaces?;

        workspaces.activate(saved_active.min(workspaces.len() - 1));

        Some(workspaces)
    }

    fn restore_tabs(
        tabs: Vec<TabState>,
        active_tab: usize,
        default_profile: (Option<String>, Vec<String>),
        next_id: &mut u64,
        cx: &mut Context<Self>,
    ) -> Option<TabManager<TerminalPaneTree>> {
        let mut restored = Vec::new();

        for mut tab_state in tabs {
            resolve_restored_launch(&mut tab_state, cx.global::<AppSettings>());

            let name = tab_state
                .name
                .clone()
                .filter(|n| !n.trim().is_empty())
                .filter(|n| tab_state.user_named || !legacy_generated_tab_title(n));

            // A saved pane layout rebuilds the split tree (one fresh shell per
            // leaf); an unusable layout degrades to the flat single-pane path.
            let tree = tab_state
                .panes
                .as_ref()
                .and_then(|panes| Self::restore_pane_node(panes, next_id, cx))
                .map(PaneTree::from_root);

            let entry = if let Some(tree) = tree {
                Some((tree, TabId(Self::alloc_id(next_id))))
            } else {
                let surface_id = Self::alloc_id(next_id);
                match TerminalPane::spawn(cx, surface_id, Some(tab_state), default_profile.clone())
                {
                    Ok(pane) => {
                        Self::watch_pane(&pane, cx);
                        Some((
                            PaneTree::new_leaf(PaneId(surface_id), pane),
                            TabId(surface_id),
                        ))
                    }
                    Err(error) => {
                        warn!("failed to restore tab {surface_id}: {error}");
                        None
                    }
                }
            };

            if let Some((tree, tab_id)) = entry {
                let default_title = tree.focused_pane().read(cx).profile_name().to_string();
                restored.push((tree, tab_id, name, default_title));
            }
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

        tab_manager.activate(active_tab.min(tab_manager.len() - 1));

        Some(tab_manager)
    }

    /// Rebuild one node of a saved pane layout, spawning a fresh shell per
    /// leaf. An unspawnable leaf is skipped and its split collapses around it
    /// (a split left with one child becomes that child); `None` when no leaf
    /// of the subtree could spawn.
    fn restore_pane_node(
        node: &PaneNodeState,
        next_id: &mut u64,
        cx: &mut Context<Self>,
    ) -> Option<PaneNode<Entity<TerminalPane>, Entity<ResizableState>>> {
        match node {
            PaneNodeState::Leaf { shell, args, cwd } => {
                let surface_id = Self::alloc_id(next_id);

                let mut launch = TabState {
                    name: None,
                    user_named: false,
                    shell: shell.clone(),
                    args: args.clone(),
                    cwd: cwd.clone(),
                    panes: None,
                };

                resolve_restored_launch(&mut launch, cx.global::<AppSettings>());

                // Spawn retries without the saved cwd internally.
                match TerminalPane::spawn(cx, surface_id, Some(launch), (None, Vec::new())) {
                    Ok(pane) => {
                        Self::watch_pane(&pane, cx);
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
                    .filter_map(|child| Self::restore_pane_node(child, next_id, cx))
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
        cx: &mut Context<Self>,
        surface_id: u64,
        default_profile: (Option<String>, Vec<String>),
        cwd: Option<String>,
    ) -> Entity<TerminalPane> {
        let launch = cwd.map(|cwd| TabState {
            shell: default_profile.0.clone(),
            args: default_profile.1.clone(),
            cwd: Some(cwd),
            ..TabState::default()
        });

        let spawned =
            TerminalPane::spawn(cx, surface_id, launch, default_profile.clone()).or_else(|error| {
                warn!("spawn with workspace cwd/profile failed, retrying default: {error}");
                TerminalPane::spawn(cx, surface_id, None, default_profile.clone())
            });

        let pane = match spawned {
            Ok(pane) => pane,
            Err(error) => {
                warn!("default profile failed, retrying built-in shell: {error}");
                TerminalPane::spawn(cx, surface_id, None, (None, Vec::new()))
                    .expect("GPUI terminal surface")
            }
        };

        Self::watch_pane(&pane, cx);

        pane
    }

    fn session_state(&self, cx: &App) -> SessionState {
        // A doomed workspace (user asked to close the last one) is skipped, so
        // it never reaches local_state; the active index is recomputed over
        // the kept workspaces.
        let mut active_workspace = 0usize;
        let mut workspaces = Vec::new();

        let default_profile = cx.global::<AppSettings>().default_profile_command();

        for workspace in self.workspaces.summaries() {
            if Some(workspace.id) == self.doomed_workspace {
                continue;
            }

            let Some(tabs) = self.workspaces.tabs_of(workspace.id) else {
                continue;
            };

            if workspace.active {
                active_workspace = workspaces.len();
            }

            workspaces.push(WorkspaceState {
                name: workspace.name,
                cwd: (!workspace.cwd.is_empty()).then_some(workspace.cwd),
                pinned: workspace.pinned,
                active_tab: tabs.active_index(),
                tabs: tabs
                    .tabs()
                    .iter()
                    .map(|tab| {
                        // Flat fields always mirror the focused pane, so a
                        // snapshot without splits stays in the old format and
                        // an old build restores something sensible from a
                        // split one.
                        let tree = tab.surface();
                        let mut state = tree.focused_pane().read(cx).tab_state();
                        normalize_saved_launch(&mut state, &default_profile);
                        state.name = tab.user_title().map(str::to_owned);
                        state.user_named = state.name.is_some();
                        state.panes = (!tree.is_single_leaf())
                            .then(|| pane_node_state(tree.root(), &default_profile, cx));
                        state
                    })
                    .collect(),
            });
        }

        SessionState {
            active_workspace,
            workspaces,
        }
    }

    pub(super) fn sync_session_memory(&self, cx: &mut Context<Self>) {
        let session = self.session_state(cx);
        if let Some(entry) = cx.global_mut::<WindowRegistry>().get_mut(self.window_id) {
            entry.session = Some(session);
        }
    }
}

#[cfg(test)]
mod launch_resolution_tests {
    use nmt_config::local_state::TabState;

    use super::{legacy_generated_tab_title, normalize_saved_launch, resolve_restored_launch};
    use crate::ui::settings::{AppSettings, Profile};

    fn settings_with_pwsh_default() -> AppSettings {
        let mut settings = AppSettings::default();
        settings.profiles = vec![
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
        ];
        settings.default_profile = "PowerShell".into();
        settings
    }

    fn tab(shell: Option<&str>, args: &[&str]) -> TabState {
        TabState {
            name: None,
            user_named: false,
            shell: shell.map(str::to_string),
            args: args.iter().map(|a| a.to_string()).collect(),
            cwd: None,
            panes: None,
        }
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
