//! A `WorkspaceManager` owns one or more workspaces; each workspace owns its own
//! [`TabManager`] plus display metadata (name, cwd). Exactly one workspace is
//! active, and the set is never empty — `close_workspace` refuses the last one, so
//! `active` always points at a real workspace (mirrors `TabManager`'s invariant).
//!
use std::{iter, path};

use nmt_agent_utils::AgentRuntimeStatus;
use nmt_i18n::i18n;

use crate::tabs::{CommandOutcome, TabId, TabManager};
use crate::ui::{ActiveList, HasId, TabSurface};
use crate::workspace::roots::path_identity;

mod roots;

pub use crate::workspace::roots::{RootChange, WorkspaceRoots, root_identity};

/// What a workspace entry reports about the terminals inside it, folded over
/// every tab it owns.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TerminalActivity {
    #[default]
    Idle,
    /// A command is executing right now.
    Running,
    /// A command ended in a background tab and the user has not looked yet.
    Finished(CommandOutcome),
}

impl TerminalActivity {
    /// Rank for [`Self::merge`]. An unacknowledged result outranks live output
    /// because it is the part the user has not seen, and a failure outranks a
    /// success for the same reason.
    fn rank(self) -> u8 {
        match self {
            Self::Idle => 0,
            Self::Running => 1,
            Self::Finished(CommandOutcome::Succeeded) => 2,
            Self::Finished(CommandOutcome::Failed) => 3,
        }
    }

    /// Fold one more tab's activity into the workspace entry's single slot.
    pub fn merge(self, other: Self) -> Self {
        if other.rank() > self.rank() {
            other
        } else {
            self
        }
    }
}

/// Work behind a workspace entry's progress bar, folded over everything inside
/// it that reports progress. Counted in percent points so contributors on
/// different scales add up: one unit of work is 100 points, whether it is a
/// terminal command reporting 40% or one item of an agent's task list.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProgressTally {
    done: u32,
    total: u32,
}

impl ProgressTally {
    /// One unit of work that is `percent` complete.
    pub fn percent(percent: u8) -> Self {
        Self {
            done: u32::from(percent).min(100),
            total: 100,
        }
    }

    /// `done` finished units out of `total`.
    pub fn tasks(done: u32, total: u32) -> Self {
        Self {
            done: done.min(total) * 100,
            total: total * 100,
        }
    }

    /// Fold one more contributor into the workspace entry's single slot.
    pub fn merge(self, other: Self) -> Self {
        Self {
            done: self.done + other.done,
            total: self.total + other.total,
        }
    }

    /// Share of the tallied work that is done, or `None` while nothing in the
    /// workspace reports progress at all.
    pub fn fraction(self) -> Option<f32> {
        (self.total > 0).then(|| self.done as f32 / self.total as f32)
    }
}

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
    /// Where the workspace lives. `None` only for the Settings pseudo
    /// workspace, which is a view of the configuration file and has no
    /// filesystem location at all.
    roots: Option<WorkspaceRoots>,
    pinned: bool,
    /// A workspace the user has not adopted yet: it stays out of the saved
    /// session, so opening a directory to run one command leaves nothing
    /// behind. Clearing the flag is what promotes it to saved work.
    temporary: bool,
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

pub fn default_workspace_name() -> &'static str {
    i18n("shell-workspace-default-name")
}

/// Every directory a summary owns, primary first, skipping placeholder
/// entries that do not name a concrete filesystem location.
fn summary_roots(ws: &WorkspaceSummary) -> impl Iterator<Item = (bool, Vec<String>)> {
    iter::once((true, ws.cwd.as_str()))
        .chain(ws.additional_cwds.iter().map(|cwd| (false, cwd.as_str())))
        .filter_map(|(primary, cwd)| Some((primary, root_identity(cwd)?)))
}

/// The first workspace that owns exactly `target` as one of its directories.
/// A primary directory outranks an additional directory that identifies the
/// same path, so the workspace whose defaults live there wins the tie.
/// Comparison uses the same case-insensitive, separator-agnostic component
/// identity as [`best_match`], while placeholder cwds remain ineligible.
pub fn exact_match(summaries: &[WorkspaceSummary], target: &path::Path) -> Option<WorkspaceId> {
    let target = path_identity(target);
    if target.is_empty() {
        return None;
    }

    let mut additional: Option<WorkspaceId> = None;
    for ws in summaries {
        for (primary, comps) in summary_roots(ws) {
            if comps != target {
                continue;
            }
            if primary {
                return Some(ws.id);
            }
            additional = additional.or(Some(ws.id));
        }
    }
    additional
}

/// The workspace owning a directory that is an ancestor of (or equal to)
/// `target`. Candidates rank by longest matching root in path components, then
/// by a primary directory over an additional one, then by workspace order.
/// Workspaces whose only directories are empty or `"."` never match because
/// they do not identify a concrete filesystem location.
pub fn best_match(summaries: &[WorkspaceSummary], target: &path::Path) -> Option<WorkspaceId> {
    let target = path_identity(target);
    let mut best: Option<(usize, bool, WorkspaceId)> = None;
    for ws in summaries {
        for (primary, comps) in summary_roots(ws) {
            if comps.len() > target.len() || !comps.iter().zip(&target).all(|(a, b)| a == b) {
                continue;
            }
            let better = best.is_none_or(|(len, was_primary, _)| {
                comps.len() > len || (comps.len() == len && primary && !was_primary)
            });
            if better {
                best = Some((comps.len(), primary, ws.id));
            }
        }
    }
    best.map(|(_, _, id)| id)
}

/// Chrome-facing summary of a workspace (presentation-agnostic).
pub struct WorkspaceSummary {
    pub id: WorkspaceId,
    pub name: String,
    /// The primary directory, or `""` for the location-free Settings entry.
    pub cwd: String,
    /// Directories owned beyond `cwd`, in workspace order.
    pub additional_cwds: Vec<String>,
    pub active: bool,
    pub agent_status: AgentRuntimeStatus,
    /// Terminal activity across this workspace's tabs. The manager cannot see
    /// pane state, so the shell fills this in when it projects the summaries
    /// for the chrome.
    pub terminal_activity: TerminalActivity,
    pub unread_count: usize,
    pub latest_unread_text: Option<String>,
    pub pinned: bool,
    pub closeable: bool,
    /// Not part of the saved session until the user activates it.
    pub temporary: bool,
    pub kind: WorkspaceKind,
    /// Work reported inside this workspace. The manager can only see what its
    /// tabs report over OSC 9;4; the shell folds in the agent panes' task
    /// lists, which live behind entities the manager cannot read.
    pub progress: ProgressTally,
}

/// OSC 9;4 progress of a workspace's tabs. Only tabs carrying a number take
/// part — an indeterminate report has no percentage to add, and a tab without a
/// running command has no progress at all, so neither drags the bar down while
/// the others advance.
fn tabs_progress(tabs: &TabManager<TabSurface>) -> ProgressTally {
    tabs.tabs()
        .iter()
        .filter_map(|tab| tab.progress())
        .filter_map(|report| report.progress)
        .map(ProgressTally::percent)
        .fold(ProgressTally::default(), ProgressTally::merge)
}

impl WorkspaceManager {
    /// Start with a single active workspace. There is no empty state.
    pub fn new(
        tabs: TabManager<TabSurface>,
        id: WorkspaceId,
        name: String,
        roots: WorkspaceRoots,
    ) -> Self {
        Self {
            workspaces: ActiveList::new(Workspace {
                id,
                name,
                roots: Some(roots),
                pinned: false,
                temporary: false,
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
        roots: WorkspaceRoots,
    ) -> WorkspaceId {
        self.new_workspace_of_kind(tabs, id, name, Some(roots), WorkspaceKind::Normal)
    }

    /// Append a workspace of an explicit kind and make it active.
    pub fn new_workspace_of_kind(
        &mut self,
        tabs: TabManager<TabSurface>,
        id: WorkspaceId,
        name: String,
        roots: Option<WorkspaceRoots>,
        kind: WorkspaceKind,
    ) -> WorkspaceId {
        self.workspaces.push_active(Workspace {
            id,
            name,
            roots,
            pinned: false,
            temporary: false,
            kind,
            tabs,
        });
        id
    }

    /// Mark whether the workspace with `id` stays out of the saved session.
    pub fn set_temporary(&mut self, id: WorkspaceId, temporary: bool) {
        if let Some(workspace) = self.workspaces.find_mut(id) {
            workspace.temporary = temporary;
        }
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
        roots: WorkspaceRoots,
        pinned: bool,
    ) -> WorkspaceId {
        let id = self.new_workspace(tabs, id, name, roots);
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

    /// The active workspace's primary directory, or `""` for the Settings
    /// entry. New terminals, new Agent Tabs, generated labels, relative-path
    /// resolution, and Git discovery all anchor here; additional directories
    /// widen access without displacing these defaults.
    pub fn active_cwd(&self) -> &str {
        self.workspaces
            .active()
            .roots
            .as_ref()
            .map_or("", WorkspaceRoots::primary)
    }

    /// Every directory the active workspace owns, primary first.
    pub fn active_roots(&self) -> Option<&WorkspaceRoots> {
        self.workspaces.active().roots.as_ref()
    }

    /// Every directory the workspace with `id` owns, primary first.
    pub fn roots_of(&self, id: WorkspaceId) -> Option<&WorkspaceRoots> {
        self.workspaces.find(id)?.roots.as_ref()
    }

    /// Replace the directories of the workspace with `id`. The Settings
    /// entry keeps its location-free state.
    pub fn set_roots(&mut self, id: WorkspaceId, roots: WorkspaceRoots) {
        if let Some(workspace) = self.workspaces.find_mut(id)
            && workspace.kind == WorkspaceKind::Normal
        {
            workspace.roots = Some(roots);
        }
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

    /// Id of the workspace that holds `tab_id`. The sidebar lists the tabs of
    /// every workspace, so a tab command can name a tab the user is not
    /// looking at.
    pub fn workspace_of_tab(&self, tab_id: TabId) -> Option<WorkspaceId> {
        self.workspaces
            .items()
            .iter()
            .find(|ws| ws.tabs.find(tab_id).is_some())
            .map(|ws| ws.id)
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
                cwd: ws
                    .roots
                    .as_ref()
                    .map_or(String::new(), |roots| roots.primary().to_string()),
                additional_cwds: ws
                    .roots
                    .as_ref()
                    .map_or_else(Vec::new, |roots| roots.additional().to_vec()),
                active: index == self.workspaces.active_index(),
                agent_status: AgentRuntimeStatus::Idle,
                terminal_activity: TerminalActivity::Idle,
                unread_count: 0,
                latest_unread_text: None,
                pinned: ws.pinned,
                closeable: (closeable || ws.kind == WorkspaceKind::Settings) && !ws.pinned,
                temporary: ws.temporary,
                kind: ws.kind,
                progress: tabs_progress(&ws.tabs),
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
mod tests;
