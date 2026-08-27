//! Invariant: a `TabManager` is never empty. `close` refuses the last tab, so
//! `active` always points at a real tab. The ordering/activation mechanics
//! live in [`ActiveList`]; this module adds the tab-specific parts (titles,
//! exit flags, surface access).

use nmt_terminal::event::{ProgressReport, ProgressState};

use crate::ui::{ActiveList, HasId};

/// Stable per-tab identity. Survives close/reorder (index changes, id does not);
/// wake routing, titles, and drag all key off it. Derived from the shell's
/// monotonic surface-id source (same value as the surface's route/atlas id).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TabId(pub u64);

/// How an integrated-shell command ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandOutcome {
    Succeeded,
    Failed,
}

impl CommandOutcome {
    /// Grade an OSC 133 `;D` exit code. A shell that reports no code offers no
    /// evidence of failure, so silence reads as success rather than flagging
    /// every command from a partially integrated shell.
    pub fn from_exit_code(exit_code: Option<i32>) -> Self {
        match exit_code {
            Some(0) | None => Self::Succeeded,
            Some(_) => Self::Failed,
        }
    }
}

pub struct Tab<S> {
    id: TabId,
    surface: S,
    default_title: String,
    user_title: Option<String>,
    terminal_title: Option<String>,
    exited: bool,
    /// A background tab rang the bell. Cleared when the tab is focused, which
    /// is the acknowledgement — a bell on the tab you are already looking at
    /// never sets this, so no timer is needed to expire it.
    bell: bool,
    /// Result of the last command that finished while this tab sat in the
    /// background. Cleared on activation, on the same acknowledgement grounds
    /// as `bell`: a command that ends in front of the user already shows its
    /// own output and exit code.
    last_outcome: Option<CommandOutcome>,
    /// Latest OSC 9;4 report from any pane in the tab; `None` once the command
    /// clears it (state 0).
    progress: Option<ProgressReport>,
}

impl<S> HasId for Tab<S> {
    type Id = TabId;
    fn id(&self) -> TabId {
        self.id
    }
}

impl<S> Tab<S> {
    pub fn id(&self) -> TabId {
        self.id
    }

    fn new(surface: S, id: TabId, default_title: String) -> Self {
        Self {
            id,
            surface,
            default_title,
            user_title: None,
            terminal_title: None,
            exited: false,
            bell: false,
            last_outcome: None,
            progress: None,
        }
    }

    pub fn title(&self) -> &str {
        self.user_title
            .as_deref()
            .or(self.terminal_title.as_deref())
            .unwrap_or(&self.default_title)
    }

    pub fn user_title(&self) -> Option<&str> {
        self.user_title.as_deref()
    }

    pub fn exited(&self) -> bool {
        self.exited
    }

    pub fn bell(&self) -> bool {
        self.bell
    }

    pub fn last_outcome(&self) -> Option<CommandOutcome> {
        self.last_outcome
    }

    pub fn progress(&self) -> Option<ProgressReport> {
        self.progress
    }

    pub fn surface(&self) -> &S {
        &self.surface
    }

    pub fn surface_mut(&mut self) -> &mut S {
        &mut self.surface
    }
}

pub struct TabManager<S> {
    tabs: ActiveList<Tab<S>>,
}

impl<S> TabManager<S> {
    /// Start with a single active tab. There is no empty state.
    pub fn new(surface: S, id: TabId, default_title: String) -> Self {
        Self {
            tabs: ActiveList::new(Tab::new(surface, id, default_title)),
        }
    }

    /// Append a tab with its profile-derived default name and make it active.
    pub fn new_tab(&mut self, surface: S, id: TabId, default_title: String) -> TabId {
        self.tabs.push_active(Tab::new(surface, id, default_title));

        id
    }

    /// Close the tab with `id`, returning its surface so the caller can drop it
    /// (releasing the PTY/IO thread). Refuses the last tab (`None`). After closing
    /// the active tab the active falls to the right neighbor, or the left when
    /// there is no right neighbor.
    pub fn close(&mut self, id: TabId) -> Option<S> {
        self.tabs.close(id).map(|tab| tab.surface)
    }

    /// Activate by position. Out-of-range indices are ignored.
    pub fn activate(&mut self, index: usize) {
        self.tabs.activate(index);
    }

    pub fn focus_next(&mut self) {
        self.tabs.focus_next();
    }

    pub fn focus_prev(&mut self) {
        self.tabs.focus_prev();
    }

    /// Move the tab at `from` to position `to`, keeping the same tab active.
    /// No-op for out-of-range or equal indices.
    pub fn reorder(&mut self, from: usize, to: usize) {
        self.tabs.reorder(from, to);
    }

    /// Set the terminal-supplied title. An empty OSC title restores the default,
    /// while an explicit user title remains authoritative.
    pub fn set_title(&mut self, id: TabId, title: String) -> bool {
        let Some(tab) = self.tabs.find_mut(id) else {
            return false;
        };

        let previous = tab.title().to_string();

        tab.terminal_title = (!title.is_empty()).then_some(title);

        tab.title() != previous
    }

    /// Set the user-authored title, which takes precedence over OSC updates.
    pub fn rename(&mut self, id: TabId, title: String) {
        if let Some(tab) = self.tabs.find_mut(id) {
            tab.user_title = Some(title);
        }
    }

    /// Mark a tab's process as exited (read-only). Ignored if the id is gone.
    pub fn mark_exited(&mut self, id: TabId) {
        if let Some(tab) = self.tabs.find_mut(id) {
            tab.exited = true;
        }
    }

    /// Flag a bell on a background tab.
    pub fn ring_bell(&mut self, id: TabId) {
        if let Some(tab) = self.tabs.find_mut(id) {
            tab.bell = true;
        }
    }

    /// Clear the active tab's bell; returns whether anything changed, so the
    /// caller can skip a repaint. Focusing a tab is the acknowledgement.
    pub fn clear_active_bell(&mut self) -> bool {
        let tab = self.tabs.active_mut();
        let rang = tab.bell;

        tab.bell = false;

        rang
    }

    /// Record how a background tab's command ended. A failure outranks a
    /// success so a run of quick commands after a failing one cannot bury it
    /// before the user looks.
    pub fn record_outcome(&mut self, id: TabId, outcome: CommandOutcome) {
        if let Some(tab) = self.tabs.find_mut(id)
            && tab.last_outcome != Some(CommandOutcome::Failed)
        {
            tab.last_outcome = Some(outcome);
        }
    }

    /// Clear the active tab's command outcome; returns whether anything
    /// changed, so the caller can skip a repaint.
    pub fn clear_active_outcome(&mut self) -> bool {
        self.tabs.active_mut().last_outcome.take().is_some()
    }

    /// Record an OSC 9;4 report. Panes in one tab share a single bar, so the
    /// most recent report wins — a split running two progress-reporting
    /// commands shows whichever spoke last.
    pub fn set_progress(&mut self, id: TabId, report: ProgressReport) {
        if let Some(tab) = self.tabs.find_mut(id) {
            tab.progress = (report.state != ProgressState::Remove).then_some(report);
        }
    }

    /// Drop a tab's progress bar (its command is gone, so no state-0 report is
    /// coming to clear it).
    pub fn clear_progress(&mut self, id: TabId) {
        if let Some(tab) = self.tabs.find_mut(id) {
            tab.progress = None;
        }
    }

    pub fn active(&self) -> &S {
        &self.tabs.active().surface
    }

    pub fn active_mut(&mut self) -> &mut S {
        &mut self.tabs.active_mut().surface
    }

    pub fn active_id(&self) -> TabId {
        self.tabs.active_id()
    }

    pub fn active_index(&self) -> usize {
        self.tabs.active_index()
    }

    pub fn len(&self) -> usize {
        self.tabs.len()
    }

    pub fn tabs(&self) -> &[Tab<S>] {
        self.tabs.items()
    }

    /// Find a tab by id (host-event routing). Index may have shifted; id is stable.
    pub fn find(&self, id: TabId) -> Option<&Tab<S>> {
        self.tabs.find(id)
    }

    pub fn find_mut(&mut self, id: TabId) -> Option<&mut Tab<S>> {
        self.tabs.find_mut(id)
    }
}

#[cfg(test)]
mod tests;
