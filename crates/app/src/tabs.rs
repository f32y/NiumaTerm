//! Multi-session tab model: a set of tabs plus the active index, with all
//! lifecycle logic (new / close / switch / reorder / title / exited) in one
//! place. Pure logic — generic over the surface type `S` so it unit-tests
//! without starting a PTY.
//!
//! Invariant: a `TabManager` is never empty. `close` refuses the last tab, so
//! `active` always points at a real tab.

/// Stable per-tab identity. Survives close/reorder (index changes, id does not);
/// wake routing, titles, and drag all key off it. Derived from the shell's
/// monotonic surface-id source (same value as the surface's route/atlas id).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TabId(pub u64);

pub struct Tab<S> {
    id: TabId,
    surface: S,
    default_title: String,
    user_title: Option<String>,
    terminal_title: Option<String>,
    exited: bool,
}

impl<S> Tab<S> {
    pub fn id(&self) -> TabId {
        self.id
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
    pub fn surface(&self) -> &S {
        &self.surface
    }
    pub fn surface_mut(&mut self) -> &mut S {
        &mut self.surface
    }
}

pub struct TabManager<S> {
    tabs: Vec<Tab<S>>,
    active: usize,
}

impl<S> TabManager<S> {
    /// Start with a single active tab. There is no empty state.
    pub fn new(surface: S, id: TabId, default_title: String) -> Self {
        Self {
            tabs: vec![Tab {
                id,
                surface,
                default_title,
                user_title: None,
                terminal_title: None,
                exited: false,
            }],
            active: 0,
        }
    }

    /// Append a tab with its profile-derived default name and make it active.
    pub fn new_tab(&mut self, surface: S, id: TabId, default_title: String) -> TabId {
        self.tabs.push(Tab {
            id,
            surface,
            default_title,
            user_title: None,
            terminal_title: None,
            exited: false,
        });
        self.active = self.tabs.len() - 1;
        id
    }

    /// Close the tab with `id`, returning its surface so the caller can drop it
    /// (releasing the PTY/IO thread). Refuses the last tab (`None`). After closing
    /// the active tab the active falls to the right neighbor, or the left when
    /// there is no right neighbor.
    pub fn close(&mut self, id: TabId) -> Option<S> {
        if self.tabs.len() <= 1 {
            return None;
        }
        let idx = self.index_of(id)?;
        let removed = self.tabs.remove(idx);
        if idx < self.active {
            self.active -= 1;
        } else if idx == self.active {
            // idx now points at the former right neighbor; clamp to the left
            // neighbor when the closed tab was last.
            self.active = idx.min(self.tabs.len() - 1);
        }
        Some(removed.surface)
    }

    /// Activate by position. Out-of-range indices are ignored.
    pub fn activate(&mut self, index: usize) {
        if index < self.tabs.len() {
            self.active = index;
        }
    }

    pub fn focus_next(&mut self) {
        self.active = (self.active + 1) % self.tabs.len();
    }

    pub fn focus_prev(&mut self) {
        let n = self.tabs.len();
        self.active = (self.active + n - 1) % n;
    }

    /// Move the tab at `from` to position `to`, keeping the same tab active.
    /// No-op for out-of-range or equal indices.
    pub fn reorder(&mut self, from: usize, to: usize) {
        let n = self.tabs.len();
        if from >= n || to >= n || from == to {
            return;
        }
        let active_id = self.tabs[self.active].id;
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);
        // Active follows its tab by id.
        self.active = self.index_of(active_id).unwrap_or(self.active);
    }

    /// Set the terminal-supplied title. An empty OSC title restores the default,
    /// while an explicit user title remains authoritative.
    pub fn set_title(&mut self, id: TabId, title: String) -> bool {
        let Some(tab) = self.find_mut(id) else {
            return false;
        };
        let previous = tab.title().to_string();
        tab.terminal_title = (!title.is_empty()).then_some(title);
        tab.title() != previous
    }

    /// Set the user-authored title, which takes precedence over OSC updates.
    pub fn rename(&mut self, id: TabId, title: String) {
        if let Some(tab) = self.find_mut(id) {
            tab.user_title = Some(title);
        }
    }

    /// Mark a tab's process as exited (read-only). Ignored if the id is gone.
    pub fn mark_exited(&mut self, id: TabId) {
        if let Some(idx) = self.index_of(id) {
            self.tabs[idx].exited = true;
        }
    }

    pub fn active(&self) -> &S {
        &self.tabs[self.active].surface
    }

    #[allow(unused)]
    pub fn active_mut(&mut self) -> &mut S {
        &mut self.tabs[self.active].surface
    }

    pub fn active_id(&self) -> TabId {
        self.tabs[self.active].id
    }

    pub fn active_index(&self) -> usize {
        self.active
    }

    pub fn len(&self) -> usize {
        self.tabs.len()
    }

    pub fn tabs(&self) -> &[Tab<S>] {
        &self.tabs
    }

    /// Find a tab by id (host-event routing). Index may have shifted; id is stable.
    pub fn find(&self, id: TabId) -> Option<&Tab<S>> {
        self.index_of(id).map(|idx| &self.tabs[idx])
    }

    pub fn find_mut(&mut self, id: TabId) -> Option<&mut Tab<S>> {
        self.index_of(id).map(|idx| &mut self.tabs[idx])
    }

    fn index_of(&self, id: TabId) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a manager of fake surfaces (u32) with sequential ids 1..=n, ids
    /// equal to the surface value for easy assertions.
    fn manager(n: u32) -> TabManager<u32> {
        let mut mgr = TabManager::new(1, TabId(1), "PowerShell".into());
        for i in 2..=n {
            mgr.new_tab(i, TabId(i as u64), "PowerShell".into());
        }
        mgr
    }

    #[test]
    fn new_tab_becomes_active() {
        let mut mgr = manager(1);
        assert_eq!(mgr.active_id(), TabId(1));
        mgr.new_tab(2, TabId(2), "PowerShell".into());
        assert_eq!(mgr.active_index(), 1);
        assert_eq!(mgr.active_id(), TabId(2));
        assert_eq!(*mgr.active(), 2);
    }

    #[test]
    fn new_tabs_use_their_profile_names() {
        let mut mgr = manager(1);
        assert_eq!(mgr.tabs()[0].title(), "PowerShell");
        mgr.new_tab(2, TabId(2), "Command Prompt".into());
        mgr.new_tab(3, TabId(3), "Developer PowerShell".into());
        assert_eq!(mgr.tabs()[1].title(), "Command Prompt");
        assert_eq!(mgr.tabs()[2].title(), "Developer PowerShell");
    }

    #[test]
    fn close_is_refused_for_single_tab() {
        let mut mgr = manager(1);
        assert!(mgr.close(TabId(1)).is_none());
        assert_eq!(mgr.len(), 1);
    }

    #[test]
    fn close_active_falls_to_right_neighbor() {
        let mut mgr = manager(3); // active = tab3 (index 2)
        mgr.activate(1); // active = tab2 (index 1)
        let removed = mgr.close(TabId(2));
        assert_eq!(removed, Some(2));
        // tab3 was to the right; it is now active at index 1.
        assert_eq!(mgr.active_id(), TabId(3));
        assert_eq!(mgr.active_index(), 1);
    }

    #[test]
    fn close_active_with_no_right_neighbor_falls_left() {
        let mut mgr = manager(3); // active = tab3 (index 2, rightmost)
        let removed = mgr.close(TabId(3));
        assert_eq!(removed, Some(3));
        assert_eq!(mgr.active_id(), TabId(2));
        assert_eq!(mgr.active_index(), 1);
    }

    #[test]
    fn closing_left_of_active_keeps_active_tab() {
        let mut mgr = manager(3);
        mgr.activate(2); // active = tab3
        mgr.close(TabId(1)); // closes a tab left of active
        assert_eq!(mgr.active_id(), TabId(3));
        assert_eq!(mgr.active_index(), 1);
    }

    #[test]
    fn focus_next_and_prev_wrap_around() {
        let mut mgr = manager(3);
        mgr.activate(2);
        mgr.focus_next();
        assert_eq!(mgr.active_index(), 0); // wrapped to first
        mgr.focus_prev();
        assert_eq!(mgr.active_index(), 2); // wrapped to last
    }

    #[test]
    fn reorder_moves_tab_and_active_follows() {
        let mut mgr = manager(3); // [t1, t2, t3], active t3
        mgr.activate(0); // active = t1
        mgr.reorder(0, 2); // move t1 to the end -> [t2, t3, t1]
        assert_eq!(mgr.tabs()[0].id(), TabId(2));
        assert_eq!(mgr.tabs()[2].id(), TabId(1));
        // active still t1, now at index 2.
        assert_eq!(mgr.active_id(), TabId(1));
        assert_eq!(mgr.active_index(), 2);
    }

    #[test]
    fn terminal_title_replaces_default_and_empty_restores_it() {
        let mut mgr = manager(2);
        assert!(mgr.set_title(TabId(1), "vim".into()));
        assert_eq!(mgr.tabs()[0].title(), "vim");
        assert_eq!(mgr.tabs()[1].title(), "PowerShell");
        assert!(mgr.set_title(TabId(1), String::new()));
        assert_eq!(mgr.tabs()[0].title(), "PowerShell");
    }

    #[test]
    fn user_title_takes_precedence_over_terminal_title() {
        let mut mgr = manager(1);
        mgr.set_title(TabId(1), "vim".into());
        mgr.rename(TabId(1), "editor".into());

        assert_eq!(mgr.tabs()[0].title(), "editor");
        assert_eq!(mgr.tabs()[0].user_title(), Some("editor"));
        assert!(!mgr.set_title(TabId(1), "shell".into()));
        assert_eq!(mgr.tabs()[0].title(), "editor");
    }

    #[test]
    fn mark_exited_keeps_tab_and_flags_it() {
        let mut mgr = manager(2);
        mgr.mark_exited(TabId(1));
        assert!(mgr.tabs()[0].exited());
        assert_eq!(mgr.len(), 2);
    }

    #[test]
    fn id_is_stable_across_close_and_reorder() {
        let mut mgr = manager(3);
        mgr.close(TabId(1)); // indices shift, ids do not
        assert_eq!(mgr.tabs()[0].id(), TabId(2));
        mgr.reorder(0, 1);
        assert_eq!(mgr.tabs()[1].id(), TabId(2));
        // tab3 keeps its id throughout.
        assert!(mgr.tabs().iter().any(|t| t.id() == TabId(3)));
    }
}
