//! Workspace grouping above tabs: Window → Workspace → Surface.
//!
//! A `WorkspaceManager` owns one or more workspaces; each workspace owns its own
//! [`TabManager`] plus display metadata (name, cwd, busy). Exactly one workspace is
//! active, and the set is never empty — `close_workspace` refuses the last one, so
//! `active` always points at a real workspace (mirrors `TabManager`'s invariant).
//!
//! Pure logic — generic over the surface type `S` so it unit-tests without a PTY.

use nmt_agent_utils::AgentRuntimeStatus;

use crate::active_list::{ActiveList, HasId};
use crate::tabs::{TabId, TabManager};

/// Stable per-workspace identity. Survives close (index changes, id does not).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WorkspaceId(pub u64);

pub struct Workspace<S> {
    id: WorkspaceId,
    name: String,
    cwd: String,
    busy: bool,
    pinned: bool,
    tabs: TabManager<S>,
}

impl<S> HasId for Workspace<S> {
    type Id = WorkspaceId;
    fn id(&self) -> WorkspaceId {
        self.id
    }
}

pub struct WorkspaceManager<S> {
    workspaces: ActiveList<Workspace<S>>,
}

pub const DEFAULT_WORKSPACE_NAME: &str = "New Workspace";

/// A path as comparable components: separators unified by `Path`, each
/// component lowercased (Windows filesystems are case-insensitive). Literal
/// comparison only — no symlink resolution, no filesystem access.
fn cmp_components(path: &std::path::Path) -> Vec<String> {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
        .collect()
}

/// The workspace whose cwd is an ancestor of (or equal to) `target`, chosen
/// by longest prefix in path components; ties go to the earlier workspace.
/// Workspaces with an empty or `"."` cwd never match because they do not identify
/// a concrete filesystem location.
pub fn best_match(summaries: &[WorkspaceSummary], target: &std::path::Path) -> Option<WorkspaceId> {
    let target = cmp_components(target);
    let mut best: Option<(usize, WorkspaceId)> = None;
    for ws in summaries {
        let cwd = ws.cwd.trim();
        if cwd.is_empty() || cwd == "." {
            continue;
        }
        let comps = cmp_components(std::path::Path::new(cwd));
        if comps.is_empty() || comps.len() > target.len() {
            continue;
        }
        if comps.iter().zip(&target).all(|(a, b)| a == b)
            && best.is_none_or(|(len, _)| comps.len() > len)
        {
            best = Some((comps.len(), ws.id));
        }
    }
    best.map(|(_, id)| id)
}

/// Chrome-facing summary of a workspace (presentation-agnostic).
pub struct WorkspaceSummary {
    pub id: WorkspaceId,
    pub name: String,
    pub cwd: String,
    pub active: bool,
    #[allow(dead_code)] // retained for compatibility while Agent status replaces its UI projection
    pub busy: bool,
    pub agent_status: AgentRuntimeStatus,
    pub unread_count: usize,
    pub latest_unread_text: Option<String>,
    pub pinned: bool,
    pub closeable: bool,
}

impl<S> WorkspaceManager<S> {
    /// Start with a single active workspace. There is no empty state.
    pub fn new(
        tabs: TabManager<S>,
        id: WorkspaceId,
        name: String,
        cwd: String,
        busy: bool,
    ) -> Self {
        Self {
            workspaces: ActiveList::new(Workspace {
                id,
                name,
                cwd,
                busy,
                pinned: false,
                tabs,
            }),
        }
    }

    /// Append a workspace (already seeded with its tab set) and make it active.
    pub fn new_workspace(
        &mut self,
        tabs: TabManager<S>,
        id: WorkspaceId,
        name: String,
        cwd: String,
        busy: bool,
    ) -> WorkspaceId {
        self.workspaces.push_active(Workspace {
            id,
            name,
            cwd,
            busy,
            pinned: false,
            tabs,
        });
        id
    }

    /// Add a restored workspace with its saved pin state.
    pub fn new_workspace_with_pinned(
        &mut self,
        tabs: TabManager<S>,
        id: WorkspaceId,
        name: String,
        cwd: String,
        busy: bool,
        pinned: bool,
    ) -> WorkspaceId {
        let id = self.new_workspace(tabs, id, name, cwd, busy);
        self.set_pinned(id, pinned);
        id
    }

    /// Toggle a workspace's pin state. Pinning moves it to the end of the
    /// pinned group; unpinning moves it to the start of the unpinned group.
    pub fn set_pinned(&mut self, id: WorkspaceId, pinned: bool) {
        let Some(idx) = self.workspaces.index_of(id) else {
            return;
        };
        if self.workspaces.items()[idx].pinned == pinned {
            return;
        }
        self.workspaces.edit_preserving_active(|workspaces| {
            let mut workspace = workspaces.remove(idx);
            workspace.pinned = pinned;
            let insert_at = workspaces
                .iter()
                .position(|ws| !ws.pinned)
                .unwrap_or(workspaces.len());
            workspaces.insert(insert_at, workspace);
        });
    }

    /// Move the workspace at `from` to `to`, keeping the same workspace active.
    /// Cross-boundary pinned/unpinned moves are ignored.
    pub fn reorder(&mut self, from: usize, to: usize) {
        let workspaces = self.workspaces.items();
        let n = workspaces.len();
        if from >= n || to >= n || workspaces[from].pinned != workspaces[to].pinned {
            return;
        }
        self.workspaces.reorder(from, to);
    }

    /// Close the workspace with `id`, returning it so the caller can drop its
    /// surfaces. Refuses the last workspace and pinned workspaces (`None`).
    /// After closing the active workspace the active falls to the right
    /// neighbour, or the left when there is no right neighbour.
    pub fn close_workspace(&mut self, id: WorkspaceId) -> Option<Workspace<S>> {
        if self.workspaces.find(id)?.pinned {
            return None;
        }
        self.workspaces.close(id)
    }

    /// Activate by position. Out-of-range indices are ignored.
    pub fn activate(&mut self, index: usize) {
        self.workspaces.activate(index);
    }

    pub fn active_tabs(&self) -> &TabManager<S> {
        &self.workspaces.active().tabs
    }

    /// The active workspace's current directory.
    pub fn active_cwd(&self) -> &str {
        &self.workspaces.active().cwd
    }

    /// Rename the workspace with `id`. Blank names and unknown ids are ignored.
    pub fn rename(&mut self, id: WorkspaceId, name: String) {
        if name.trim().is_empty() {
            return;
        }
        if let Some(workspace) = self.workspaces.find_mut(id) {
            workspace.name = name;
        }
    }

    pub fn active_tabs_mut(&mut self) -> &mut TabManager<S> {
        &mut self.workspaces.active_mut().tabs
    }

    /// The tab set of the workspace with `id`.
    pub fn tabs_of(&self, id: WorkspaceId) -> Option<&TabManager<S>> {
        self.workspaces.find(id).map(|ws| &ws.tabs)
    }

    /// Tab sets of every workspace (the window-close process sweep).
    pub fn all_tabs(&self) -> impl Iterator<Item = &TabManager<S>> {
        self.workspaces.items().iter().map(|ws| &ws.tabs)
    }

    pub fn is_pinned(&self, id: WorkspaceId) -> bool {
        self.workspaces.find(id).is_some_and(|ws| ws.pinned)
    }

    /// The tab set that contains `tab_id`, searched across all workspaces (a
    /// background workspace's surface still polls host events).
    pub fn tab_manager_for(&self, tab_id: TabId) -> Option<&TabManager<S>> {
        self.workspaces
            .items()
            .iter()
            .map(|ws| &ws.tabs)
            .find(|tabs| tabs.find(tab_id).is_some())
    }

    /// Id of the tab whose surface matches `pred`, searched across all
    /// workspaces (host events arrive from background workspaces too).
    pub fn find_tab_id(&self, pred: impl Fn(&S) -> bool) -> Option<TabId> {
        self.workspaces
            .items()
            .iter()
            .flat_map(|ws| ws.tabs.tabs())
            .find(|tab| pred(tab.surface()))
            .map(|tab| tab.id())
    }

    pub fn tab_manager_for_mut(&mut self, tab_id: TabId) -> Option<&mut TabManager<S>> {
        self.workspaces
            .items_mut()
            .iter_mut()
            .map(|ws| &mut ws.tabs)
            .find(|tabs| tabs.find(tab_id).is_some())
    }

    /// Number of workspaces (always >= 1).
    pub fn len(&self) -> usize {
        self.workspaces.len()
    }

    /// Lightweight per-workspace summary for chrome (name/active/busy), in order.
    /// A presentation-agnostic view of the workspaces for the shell chrome.
    pub fn summaries(&self) -> Vec<WorkspaceSummary> {
        let closeable = self.workspaces.len() > 1;
        self.workspaces
            .items()
            .iter()
            .enumerate()
            .map(|(index, ws)| WorkspaceSummary {
                id: ws.id,
                name: ws.name.clone(),
                cwd: ws.cwd.clone(),
                active: index == self.workspaces.active_index(),
                busy: ws.busy,
                agent_status: AgentRuntimeStatus::Idle,
                unread_count: 0,
                latest_unread_text: None,
                pinned: ws.pinned,
                closeable: closeable && !ws.pinned,
            })
            .collect()
    }

    /// Index of the active workspace.
    pub fn active_index(&self) -> usize {
        self.workspaces.active_index()
    }

    /// Id of the active workspace (the set is never empty).
    pub fn active_id(&self) -> WorkspaceId {
        self.workspaces.active_id()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A manager of `n` workspaces, each holding a one-tab `TabManager<u32>`. The
    /// workspace id and its single tab's surface value both equal the 1-based index
    /// for easy assertions.
    fn manager(n: u32) -> WorkspaceManager<u32> {
        let tabs = || TabManager::new(1u32, TabId(1), "PowerShell".into());
        let mut mgr = WorkspaceManager::new(
            tabs(),
            WorkspaceId(1),
            "Workspace 1".into(),
            "/".into(),
            false,
        );
        for i in 2..=n {
            mgr.new_workspace(
                tabs(),
                WorkspaceId(i as u64),
                format!("Workspace {i}"),
                "/".into(),
                false,
            );
        }
        mgr
    }

    #[test]
    fn starts_with_one_active_workspace() {
        let mgr = manager(1);
        assert_eq!(mgr.len(), 1);
        assert_eq!(mgr.active_index(), 0);
        assert_eq!(mgr.active_id(), WorkspaceId(1));
    }

    #[test]
    fn new_workspace_becomes_active() {
        let mut mgr = manager(1);
        mgr.new_workspace(
            TabManager::new(9u32, TabId(9), "PowerShell".into()),
            WorkspaceId(2),
            "Workspace 2".into(),
            "/".into(),
            false,
        );
        assert_eq!(mgr.active_index(), 1);
        assert_eq!(mgr.active_id(), WorkspaceId(2));
    }

    #[test]
    fn close_is_refused_for_single_workspace() {
        let mut mgr = manager(1);
        assert!(mgr.close_workspace(WorkspaceId(1)).is_none());
        assert_eq!(mgr.len(), 1);
    }

    #[test]
    fn close_active_falls_to_right_neighbour() {
        let mut mgr = manager(3); // active = ws3 (index 2)
        mgr.activate(1); // active = ws2
        let removed = mgr.close_workspace(WorkspaceId(2));
        assert!(removed.is_some());
        assert_eq!(mgr.active_id(), WorkspaceId(3));
        assert_eq!(mgr.active_index(), 1);
    }

    #[test]
    fn close_active_with_no_right_neighbour_falls_left() {
        let mut mgr = manager(3); // active = ws3 (rightmost)
        mgr.close_workspace(WorkspaceId(3));
        assert_eq!(mgr.active_id(), WorkspaceId(2));
        assert_eq!(mgr.active_index(), 1);
    }

    #[test]
    fn activate_out_of_range_is_ignored() {
        let mut mgr = manager(2);
        mgr.activate(0);
        mgr.activate(5);
        assert_eq!(mgr.active_index(), 0);
    }

    #[test]
    fn active_tabs_routes_to_active_workspace() {
        let mut mgr = manager(1);
        mgr.new_workspace(
            TabManager::new(42u32, TabId(42), "PowerShell".into()),
            WorkspaceId(2),
            "Workspace 2".into(),
            "/".into(),
            false,
        );
        assert_eq!(*mgr.active_tabs().active(), 42);
        mgr.activate(0);
        assert_eq!(*mgr.active_tabs().active(), 1);
    }

    #[test]
    fn rename_updates_name() {
        let mut mgr = manager(1);
        mgr.rename(WorkspaceId(1), "project".into());
        assert_eq!(mgr.summaries()[0].name, "project");
        // Blank and unknown-id renames are ignored.
        mgr.rename(WorkspaceId(1), "   ".into());
        mgr.rename(WorkspaceId(9), "ghost".into());
        assert_eq!(mgr.summaries()[0].name, "project");
    }

    /// Summaries with the given cwds, ids = 1-based position.
    fn summaries(cwds: &[&str]) -> Vec<WorkspaceSummary> {
        cwds.iter()
            .enumerate()
            .map(|(i, cwd)| WorkspaceSummary {
                id: WorkspaceId(i as u64 + 1),
                name: format!("Workspace {}", i + 1),
                cwd: cwd.to_string(),
                active: i == 0,
                busy: false,
                agent_status: AgentRuntimeStatus::Idle,
                unread_count: 0,
                latest_unread_text: None,
                pinned: false,
                closeable: cwds.len() > 1,
            })
            .collect()
    }

    fn matched(cwds: &[&str], target: &str) -> Option<WorkspaceId> {
        best_match(&summaries(cwds), std::path::Path::new(target))
    }

    #[test]
    fn deepest_ancestor_wins() {
        assert_eq!(
            matched(&["C:/A/B", "C:/A"], "C:/A/B/C"),
            Some(WorkspaceId(1))
        );
    }

    #[test]
    fn shallow_ancestor_matches_when_deep_does_not() {
        assert_eq!(matched(&["C:/A/B", "C:/A"], "C:/A/D"), Some(WorkspaceId(2)));
    }

    #[test]
    fn unrelated_target_matches_nothing() {
        assert_eq!(matched(&["C:/A/B", "C:/A"], "C:/E"), None);
    }

    #[test]
    fn equal_path_matches() {
        assert_eq!(matched(&["C:/A/B"], "C:/A/B"), Some(WorkspaceId(1)));
    }

    #[test]
    fn match_is_case_insensitive_and_separator_agnostic() {
        assert_eq!(matched(&["c:\\a\\b\\"], "C:/A/B/C"), Some(WorkspaceId(1)));
    }

    #[test]
    fn component_boundary_is_respected() {
        assert_eq!(matched(&["C:/A/B"], "C:/A/BC"), None);
    }

    #[test]
    fn placeholder_cwds_are_skipped() {
        assert_eq!(matched(&[".", "", "  "], "C:/A"), None);
    }

    #[test]
    fn tie_on_depth_goes_to_the_earlier_workspace() {
        assert_eq!(matched(&["C:/A", "c:/a"], "C:/A/B"), Some(WorkspaceId(1)));
    }

    fn ids(mgr: &WorkspaceManager<u32>) -> Vec<WorkspaceId> {
        mgr.summaries().into_iter().map(|ws| ws.id).collect()
    }

    #[test]
    fn pinning_moves_to_end_of_pinned_group() {
        let mut mgr = manager(4);
        mgr.set_pinned(WorkspaceId(2), true);
        mgr.set_pinned(WorkspaceId(4), true);
        assert_eq!(
            ids(&mgr),
            vec![
                WorkspaceId(2),
                WorkspaceId(4),
                WorkspaceId(1),
                WorkspaceId(3)
            ]
        );
        assert!(mgr.summaries()[0].pinned);
        assert!(!mgr.summaries()[2].pinned);
    }

    #[test]
    fn unpinning_moves_to_start_of_unpinned_group() {
        let mut mgr = manager(4);
        mgr.set_pinned(WorkspaceId(2), true);
        mgr.set_pinned(WorkspaceId(4), true);
        mgr.set_pinned(WorkspaceId(2), false);
        assert_eq!(
            ids(&mgr),
            vec![
                WorkspaceId(4),
                WorkspaceId(2),
                WorkspaceId(1),
                WorkspaceId(3)
            ]
        );
    }

    #[test]
    fn reorder_moves_within_pin_group_only() {
        let mut mgr = manager(4);
        mgr.set_pinned(WorkspaceId(2), true);
        mgr.set_pinned(WorkspaceId(4), true);
        mgr.reorder(0, 1);
        assert_eq!(
            ids(&mgr),
            vec![
                WorkspaceId(4),
                WorkspaceId(2),
                WorkspaceId(1),
                WorkspaceId(3)
            ]
        );
        mgr.reorder(2, 3);
        assert_eq!(
            ids(&mgr),
            vec![
                WorkspaceId(4),
                WorkspaceId(2),
                WorkspaceId(3),
                WorkspaceId(1)
            ]
        );
        mgr.reorder(1, 2);
        assert_eq!(
            ids(&mgr),
            vec![
                WorkspaceId(4),
                WorkspaceId(2),
                WorkspaceId(3),
                WorkspaceId(1)
            ]
        );
    }

    #[test]
    fn active_workspace_follows_pin_and_reorder() {
        let mut mgr = manager(4);
        mgr.activate(1);
        mgr.set_pinned(WorkspaceId(2), true);
        assert_eq!(mgr.active_id(), WorkspaceId(2));
        mgr.set_pinned(WorkspaceId(4), true);
        mgr.reorder(1, 0);
        assert_eq!(mgr.active_id(), WorkspaceId(2));
    }

    #[test]
    fn close_refuses_pinned_workspace() {
        let mut mgr = manager(2);
        mgr.set_pinned(WorkspaceId(1), true);
        assert!(mgr.close_workspace(WorkspaceId(1)).is_none());
        assert_eq!(mgr.len(), 2);
        assert!(!mgr.summaries()[0].closeable);
    }
}
