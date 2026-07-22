//! A `WorkspaceManager` owns one or more workspaces; each workspace owns its own
//! [`TabManager`] plus display metadata (name, cwd). Exactly one workspace is
//! active, and the set is never empty — `close_workspace` refuses the last one, so
//! `active` always points at a real workspace (mirrors `TabManager`'s invariant).
//!
use std::path;

use nmt_agent_utils::AgentRuntimeStatus;

use crate::tabs::{TabId, TabManager};
use crate::ui::{ActiveList, HasId, TerminalPaneTree};

/// Stable per-workspace identity. Survives close (index changes, id does not).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WorkspaceId(pub u64);

pub struct Workspace {
    id: WorkspaceId,
    name: String,
    cwd: String,
    pinned: bool,
    tabs: TabManager<TerminalPaneTree>,
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
}

impl WorkspaceManager {
    /// Start with a single active workspace. There is no empty state.
    pub fn new(
        tabs: TabManager<TerminalPaneTree>,
        id: WorkspaceId,
        name: String,
        cwd: String,
    ) -> Self {
        Self {
            workspaces: ActiveList::new(Workspace {
                id,
                name,
                cwd,
                pinned: false,
                tabs,
            }),
        }
    }

    /// Append a workspace (already seeded with its tab set) and make it active.
    pub fn new_workspace(
        &mut self,
        tabs: TabManager<TerminalPaneTree>,
        id: WorkspaceId,
        name: String,
        cwd: String,
    ) -> WorkspaceId {
        self.workspaces.push_active(Workspace {
            id,
            name,
            cwd,
            pinned: false,
            tabs,
        });
        id
    }

    /// Add a restored workspace with its saved pin state.
    pub fn new_workspace_with_pinned(
        &mut self,
        tabs: TabManager<TerminalPaneTree>,
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

    pub fn active_tabs(&self) -> &TabManager<TerminalPaneTree> {
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

    pub fn active_tabs_mut(&mut self) -> &mut TabManager<TerminalPaneTree> {
        &mut self.workspaces.active_mut().tabs
    }

    /// The tab set of the workspace with `id`.
    pub fn tabs_of(&self, id: WorkspaceId) -> Option<&TabManager<TerminalPaneTree>> {
        self.workspaces.find(id).map(|ws| &ws.tabs)
    }

    /// Tab sets of every workspace (the window-close process sweep).
    pub fn all_tabs(&self) -> impl Iterator<Item = &TabManager<TerminalPaneTree>> {
        self.workspaces.items().iter().map(|ws| &ws.tabs)
    }

    pub fn is_pinned(&self, id: WorkspaceId) -> bool {
        self.workspaces.find(id).is_some_and(|ws| ws.pinned)
    }

    /// The tab set that contains `tab_id`, searched across all workspaces (a
    /// background workspace's surface still polls host events).
    pub fn tab_manager_for(&self, tab_id: TabId) -> Option<&TabManager<TerminalPaneTree>> {
        self.workspaces
            .items()
            .iter()
            .map(|ws| &ws.tabs)
            .find(|tabs| tabs.find(tab_id).is_some())
    }

    /// Id of the tab whose surface matches `pred`, searched across all
    /// workspaces (host events arrive from background workspaces too).
    pub fn find_tab_id(&self, pred: impl Fn(&TerminalPaneTree) -> bool) -> Option<TabId> {
        self.workspaces
            .items()
            .iter()
            .flat_map(|ws| ws.tabs.tabs())
            .find(|tab| pred(tab.surface()))
            .map(|tab| tab.id())
    }

    pub fn tab_manager_for_mut(
        &mut self,
        tab_id: TabId,
    ) -> Option<&mut TabManager<TerminalPaneTree>> {
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
            })
            .collect()
    }

    fn matched(cwds: &[&str], target: &str) -> Option<WorkspaceId> {
        best_match(&summaries(cwds), path::Path::new(target))
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
}
