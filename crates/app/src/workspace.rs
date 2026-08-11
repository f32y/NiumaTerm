//! A `WorkspaceManager` owns one or more workspaces; each workspace owns its own
//! [`TabManager`] plus display metadata (name, cwd). Exactly one workspace is
//! active, and the set is never empty — `close_workspace` refuses the last one, so
//! `active` always points at a real workspace (mirrors `TabManager`'s invariant).
//!
use std::path;

use nmt_agent_utils::AgentRuntimeStatus;

use crate::tabs::{TabId, TabManager};
use crate::ui::{ActiveList, HasId, TabSurface};

/// Stable per-workspace identity. Survives close (index changes, id does not).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WorkspaceId(pub u64);

/// What a workspace entry stands for. `Settings` is a pseudo workspace: it
/// holds the settings surface instead of shells, is never persisted, and is
/// excluded from the counts that decide whether a close request is about to
/// take the user's last real workspace away.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkspaceKind {
    Normal,
    Settings,
}

pub struct Workspace {
    id: WorkspaceId,
    name: String,
    cwd: String,
    pinned: bool,
    kind: WorkspaceKind,
    tabs: TabManager<TabSurface>,
}

impl HasId for Workspace {
    type Id = WorkspaceId;
    fn id(&self) -> WorkspaceId {
        self.id
    }
}

pub struct WorkspaceManager {
    workspaces: ActiveList<Workspace>,
}

pub const DEFAULT_WORKSPACE_NAME: &str = "New Workspace";

/// A path as comparable components: separators unified by `Path`, each
/// component lowercased (Windows filesystems are case-insensitive). Literal
/// comparison only — no symlink resolution, no filesystem access.
fn cmp_components(path: &path::Path) -> Vec<String> {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
        .collect()
}

/// The first workspace whose cwd identifies exactly `target`. Comparison uses
/// the same case-insensitive, separator-agnostic component identity as
/// [`best_match`], while placeholder cwds remain ineligible.
pub fn exact_match(summaries: &[WorkspaceSummary], target: &path::Path) -> Option<WorkspaceId> {
    let target = cmp_components(target);
    if target.is_empty() {
        return None;
    }

    summaries.iter().find_map(|ws| {
        let cwd = ws.cwd.trim();
        if cwd.is_empty() || cwd == "." {
            return None;
        }

        (cmp_components(path::Path::new(cwd)) == target).then_some(ws.id)
    })
}

/// The workspace whose cwd is an ancestor of (or equal to) `target`, chosen
/// by longest prefix in path components; ties go to the earlier workspace.
/// Workspaces with an empty or `"."` cwd never match because they do not identify
/// a concrete filesystem location.
pub fn best_match(summaries: &[WorkspaceSummary], target: &path::Path) -> Option<WorkspaceId> {
    let target = cmp_components(target);
    let mut best: Option<(usize, WorkspaceId)> = None;
    for ws in summaries {
        let cwd = ws.cwd.trim();
        if cwd.is_empty() || cwd == "." {
            continue;
        }
        let comps = cmp_components(path::Path::new(cwd));
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
    pub agent_status: AgentRuntimeStatus,
    pub unread_count: usize,
    pub latest_unread_text: Option<String>,
    pub pinned: bool,
    pub closeable: bool,
    pub kind: WorkspaceKind,
}

impl WorkspaceManager {
    /// Start with a single active workspace. There is no empty state.
    pub fn new(tabs: TabManager<TabSurface>, id: WorkspaceId, name: String, cwd: String) -> Self {
        Self {
            workspaces: ActiveList::new(Workspace {
                id,
                name,
                cwd,
                pinned: false,
                kind: WorkspaceKind::Normal,
                tabs,
            }),
        }
    }

    /// Append a workspace (already seeded with its tab set) and make it active.
    pub fn new_workspace(
        &mut self,
        tabs: TabManager<TabSurface>,
        id: WorkspaceId,
        name: String,
        cwd: String,
    ) -> WorkspaceId {
        self.new_workspace_of_kind(tabs, id, name, cwd, WorkspaceKind::Normal)
    }

    /// Append a workspace of an explicit kind and make it active.
    pub fn new_workspace_of_kind(
        &mut self,
        tabs: TabManager<TabSurface>,
        id: WorkspaceId,
        name: String,
        cwd: String,
        kind: WorkspaceKind,
    ) -> WorkspaceId {
        self.workspaces.push_active(Workspace {
            id,
            name,
            cwd,
            pinned: false,
            kind,
            tabs,
        });
        id
    }

    /// Id of the settings pseudo workspace, when one is open in this window.
    pub fn settings_id(&self) -> Option<WorkspaceId> {
        self.workspaces
            .items()
            .iter()
            .find(|ws| ws.kind == WorkspaceKind::Settings)
            .map(|ws| ws.id)
    }

    pub fn kind_of(&self, id: WorkspaceId) -> Option<WorkspaceKind> {
        self.workspaces.find(id).map(|ws| ws.kind)
    }

    pub fn active_kind(&self) -> WorkspaceKind {
        self.workspaces.active().kind
    }

    /// Index of the first normal workspace. The set always holds at least one,
    /// so the caller can activate the result unconditionally.
    pub fn first_normal_index(&self) -> usize {
        self.workspaces
            .items()
            .iter()
            .position(|ws| ws.kind == WorkspaceKind::Normal)
            .unwrap_or(0)
    }

    /// Workspaces that hold the user's own tabs. The last-workspace decisions
    /// count these, so an open settings entry never hides that the user is
    /// about to close everything they were working in.
    pub fn real_len(&self) -> usize {
        self.workspaces
            .items()
            .iter()
            .filter(|ws| ws.kind == WorkspaceKind::Normal)
            .count()
    }

    /// Add a restored workspace with its saved pin state.
    pub fn new_workspace_with_pinned(
        &mut self,
        tabs: TabManager<TabSurface>,
        id: WorkspaceId,
        name: String,
        cwd: String,
        pinned: bool,
    ) -> WorkspaceId {
        let id = self.new_workspace(tabs, id, name, cwd);
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
    pub fn close_workspace(&mut self, id: WorkspaceId) -> Option<Workspace> {
        if self.workspaces.find(id)?.pinned {
            return None;
        }
        self.workspaces.close(id)
    }

    /// Activate by position. Out-of-range indices are ignored.
    pub fn activate(&mut self, index: usize) {
        self.workspaces.activate(index);
    }

    pub fn active_tabs(&self) -> &TabManager<TabSurface> {
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

    pub fn active_tabs_mut(&mut self) -> &mut TabManager<TabSurface> {
        &mut self.workspaces.active_mut().tabs
    }

    /// The tab set of the workspace with `id`.
    pub fn tabs_of(&self, id: WorkspaceId) -> Option<&TabManager<TabSurface>> {
        self.workspaces.find(id).map(|ws| &ws.tabs)
    }

    /// Tab sets of every workspace (the window-close process sweep).
    pub fn all_tabs(&self) -> impl Iterator<Item = &TabManager<TabSurface>> {
        self.workspaces.items().iter().map(|ws| &ws.tabs)
    }

    pub fn is_pinned(&self, id: WorkspaceId) -> bool {
        self.workspaces.find(id).is_some_and(|ws| ws.pinned)
    }

    /// The tab set that contains `tab_id`, searched across all workspaces (a
    /// background workspace's surface still polls host events).
    pub fn tab_manager_for(&self, tab_id: TabId) -> Option<&TabManager<TabSurface>> {
        self.workspaces
            .items()
            .iter()
            .map(|ws| &ws.tabs)
            .find(|tabs| tabs.find(tab_id).is_some())
    }

    /// Id of the tab whose surface matches `pred`, searched across all
    /// workspaces (host events arrive from background workspaces too).
    pub fn find_tab_id(&self, pred: impl Fn(&TabSurface) -> bool) -> Option<TabId> {
        self.workspaces
            .items()
            .iter()
            .flat_map(|ws| ws.tabs.tabs())
            .find(|tab| pred(tab.surface()))
            .map(|tab| tab.id())
    }

    pub fn tab_manager_for_mut(&mut self, tab_id: TabId) -> Option<&mut TabManager<TabSurface>> {
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

    /// Lightweight per-workspace summary for chrome (name/active), in order.
    /// A presentation-agnostic view of the workspaces for the shell chrome.
    pub fn summaries(&self) -> Vec<WorkspaceSummary> {
        // The settings entry is always dismissible; a normal workspace stays
        // closeable only while another normal one would remain.
        let closeable = self.real_len() > 1;
        self.workspaces
            .items()
            .iter()
            .enumerate()
            .map(|(index, ws)| WorkspaceSummary {
                id: ws.id,
                name: ws.name.clone(),
                cwd: ws.cwd.clone(),
                active: index == self.workspaces.active_index(),
                agent_status: AgentRuntimeStatus::Idle,
                unread_count: 0,
                latest_unread_text: None,
                pinned: ws.pinned,
                closeable: (closeable || ws.kind == WorkspaceKind::Settings) && !ws.pinned,
                kind: ws.kind,
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

    /// Summaries with the given cwds, ids = 1-based position.
    fn summaries(cwds: &[&str]) -> Vec<WorkspaceSummary> {
        cwds.iter()
            .enumerate()
            .map(|(i, cwd)| WorkspaceSummary {
                id: WorkspaceId(i as u64 + 1),
                name: format!("Workspace {}", i + 1),
                cwd: cwd.to_string(),
                active: i == 0,
                agent_status: AgentRuntimeStatus::Idle,
                unread_count: 0,
                latest_unread_text: None,
                pinned: false,
                closeable: cwds.len() > 1,
                kind: WorkspaceKind::Normal,
            })
            .collect()
    }

    /// A manager holding `normal` normal workspaces (ids 1..=normal), with a
    /// settings entry appended last when `settings` is set (id 100).
    fn manager(normal: u64, settings: bool) -> WorkspaceManager {
        let tabs = || {
            TabManager::new(
                TabSurface::Pending(Box::default()),
                TabId(0),
                "Tab".to_string(),
            )
        };

        let mut manager = WorkspaceManager::new(
            tabs(),
            WorkspaceId(1),
            "Workspace 1".to_string(),
            "C:/one".to_string(),
        );

        for id in 2..=normal {
            manager.new_workspace(
                tabs(),
                WorkspaceId(id),
                format!("Workspace {id}"),
                format!("C:/{id}"),
            );
        }

        if settings {
            manager.new_workspace_of_kind(
                TabManager::new(TabSurface::Settings, TabId(100), "Settings".to_string()),
                WorkspaceId(100),
                "Settings".to_string(),
                String::new(),
                WorkspaceKind::Settings,
            );
        }

        manager
    }

    #[test]
    fn settings_entry_is_left_out_of_the_real_count() {
        let manager = manager(1, true);

        assert_eq!(manager.len(), 2);
        assert_eq!(manager.real_len(), 1);
        assert_eq!(manager.settings_id(), Some(WorkspaceId(100)));
        assert_eq!(manager.kind_of(WorkspaceId(1)), Some(WorkspaceKind::Normal));
    }

    #[test]
    fn a_lone_normal_workspace_stays_closed_off_beside_settings() {
        let summaries = manager(1, true).summaries();

        // The one real workspace routes into the quit/replace decision rather
        // than closing outright; the settings entry always closes.
        assert!(!summaries[0].closeable);
        assert!(summaries[1].closeable);
    }

    #[test]
    fn settings_closes_without_taking_the_last_slot() {
        let mut manager = manager(1, true);

        assert!(manager.close_workspace(WorkspaceId(100)).is_some());
        assert_eq!(manager.len(), 1);
        assert_eq!(manager.settings_id(), None);
        assert_eq!(manager.active_id(), WorkspaceId(1));
    }

    #[test]
    fn settings_reorders_like_any_other_entry() {
        let mut manager = manager(2, true);

        manager.reorder(2, 0);

        let order: Vec<_> = manager.summaries().into_iter().map(|ws| ws.id).collect();

        assert_eq!(
            order,
            vec![WorkspaceId(100), WorkspaceId(1), WorkspaceId(2)]
        );
        // The settings entry was active before the move and stays active.
        assert_eq!(manager.active_id(), WorkspaceId(100));
    }

    #[test]
    fn leaving_settings_lands_on_a_normal_workspace() {
        let mut manager = manager(2, true);

        assert_eq!(manager.active_kind(), WorkspaceKind::Settings);

        manager.activate(manager.first_normal_index());

        assert_eq!(manager.active_id(), WorkspaceId(1));
        assert_eq!(manager.active_kind(), WorkspaceKind::Normal);
    }

    fn matched(cwds: &[&str], target: &str) -> Option<WorkspaceId> {
        best_match(&summaries(cwds), path::Path::new(target))
    }

    fn exactly_matched(cwds: &[&str], target: &str) -> Option<WorkspaceId> {
        exact_match(&summaries(cwds), path::Path::new(target))
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

    #[test]
    fn exact_match_reuses_the_same_workspace_path() {
        assert_eq!(
            exactly_matched(&["C:/A", "c:\\work\\project\\"], "C:/WORK/PROJECT"),
            Some(WorkspaceId(2))
        );
    }

    #[test]
    fn exact_match_does_not_reuse_ancestor_workspace() {
        assert_eq!(exactly_matched(&["C:/A"], "C:/A/child"), None);
    }
}
