use nmt_i18n::i18n;

use crate::ui::persistence::spawn_default_pane;
use crate::ui::shell::*;

impl Shell {
    pub(crate) fn set_workspace_pinned(
        &mut self,
        id: WorkspaceId,
        pinned: bool,
        cx: &mut Context<Self>,
    ) {
        self.workspaces.set_pinned(id, pinned);

        self.sync_session_memory(cx);

        cx.notify();
    }

    pub(crate) fn reorder_workspaces(
        &mut self,
        from: usize,
        to: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.workspaces.reorder(from, to);

        self.focus_active(window, cx);

        self.sync_session_memory(cx);

        cx.notify();
    }

    /// Move a tab within the workspace that owns it. The tab id picks the tab
    /// manager, so this reaches a workspace the user is not currently in.
    pub(in crate::ui) fn reorder_tab(
        &mut self,
        tab: TabId,
        from: usize,
        to: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tabs) = self.workspaces.tab_manager_for_mut(tab) else {
            return;
        };

        tabs.reorder(from, to);

        self.focus_active(window, cx);

        self.sync_session_memory(cx);

        cx.notify();
    }

    pub(super) fn on_next_tab(&mut self, _: &NextTab, window: &mut Window, cx: &mut Context<Self>) {
        self.workspaces.active_tabs_mut().focus_next();

        self.focus_active(window, cx);

        self.sync_session_memory(cx);

        cx.notify();
    }

    pub(super) fn on_prev_tab(&mut self, _: &PrevTab, window: &mut Window, cx: &mut Context<Self>) {
        self.workspaces.active_tabs_mut().focus_prev();

        self.focus_active(window, cx);

        self.sync_session_memory(cx);

        cx.notify();
    }

    /// Position of the next tab `marked` accepts, searching after the active
    /// tab and wrapping in workspace-then-tab order so repeated jumps walk the
    /// whole marked set in turn.
    fn next_marked_tab(&self, marked: impl Fn(&Tab<TabSurface>) -> bool) -> Option<(usize, usize)> {
        let (positions, marks): (Vec<(usize, usize)>, Vec<bool>) = self
            .workspaces
            .all_tabs()
            .enumerate()
            .flat_map(|(workspace_index, tabs)| {
                tabs.tabs()
                    .iter()
                    .enumerate()
                    .map(move |(tab_index, tab)| ((workspace_index, tab_index), tab))
            })
            .map(|(position, tab)| (position, marked(tab)))
            .unzip();

        let active = (
            self.workspaces.active_index(),
            self.workspaces.active_tabs().active_index(),
        );
        let active_position = positions
            .iter()
            .position(|&position| position == active)
            .unwrap_or(0);

        next_marked_position(&marks, active_position).map(|index| positions[index])
    }

    /// The next tab holding something the user has not looked at: a background
    /// command that finished, or an unread agent reply.
    ///
    /// Each jump shrinks the set rather than advancing a cursor of its own,
    /// because focusing a tab is what clears both marks.
    pub(super) fn next_ready_tab(&self, cx: &App) -> Option<(usize, usize)> {
        self.next_marked_tab(|tab| {
            let routes = Self::agent_routes_in_surface(tab.surface(), cx);

            tab.last_outcome().is_some() || self.agent_monitor.project(&routes).unread_count > 0
        })
    }

    /// The next tab with work still in flight: a terminal running a command, or
    /// an agent still producing its answer.
    ///
    /// Unlike the ready set this one does not shrink when the tab is focused —
    /// watching a tab does not finish its work — so the jump keeps cycling
    /// while the same tabs stay busy, which is what following several parallel
    /// runs needs.
    pub(super) fn next_busy_tab(&self, cx: &App) -> Option<(usize, usize)> {
        self.next_marked_tab(|tab| {
            if Self::tab_terminal_activity(tab, cx) == TerminalActivity::Running {
                return true;
            }

            let routes = Self::agent_routes_in_surface(tab.surface(), cx);

            self.agent_monitor.project(&routes).status == AgentRuntimeStatus::Running
        })
    }

    pub(super) fn jump_to_tab(
        &mut self,
        workspace_index: usize,
        tab_index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.workspaces.activate(workspace_index);
        self.workspaces.active_tabs_mut().activate(tab_index);

        self.focus_active(window, cx);

        self.sync_session_memory(cx);

        cx.notify();
    }

    /// Open the new-workspace dialog: a name plus the shared directory editor,
    /// so a workspace can be created with several directories in one step.
    /// Confirming creates the workspace; cancel creates nothing.
    pub(crate) fn on_new_workspace(
        &mut self,
        _: &NewWorkspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(i18n("shell-workspace-default-name").to_string())
        });

        // A new workspace starts with no directory at all, which the editor's
        // non-empty invariant cannot express; the picker fills the first one
        // in and Create stays refused until it does.
        let dirs = cx.new(|cx| WorkspaceDirsEditor::new(None, cx));

        let shell = cx.entity();

        window.open_dialog(cx, move |dialog, window, _| {
            let name_input = name_input.clone();
            let dirs = dirs.clone();
            let content_name = name_input.clone();
            let content_dirs = dirs.clone();
            let shell = shell.clone();
            let margin_top = ((window.viewport_size().height - px(300.)) * 0.5).max(px(16.));

            dialog
                .title(i18n("shell-workspace-new-title"))
                .overlay_closable(false)
                .margin_top(margin_top)
                .button_props(
                    DialogButtonProps::default()
                        .ok_text(i18n("shell-workspace-create"))
                        .cancel_text(i18n("shell-workspace-cancel"))
                        .show_cancel(true),
                )
                // Plain `Dialog` never renders `button_props` buttons (only
                // `AlertDialog` does), so the footer supplies them; the
                // wrappers dispatch Confirm/CancelDialog into on_ok/on_cancel.
                .footer(
                    DialogFooter::new()
                        .child(
                            DialogAction::new().child(
                                Button::new("create-ws")
                                    .min_w(DIALOG_BUTTON_MIN_WIDTH)
                                    .label(i18n("shell-workspace-create"))
                                    .primary(),
                            ),
                        )
                        .child(
                            DialogClose::new().child(
                                Button::new("cancel-ws")
                                    .min_w(DIALOG_BUTTON_MIN_WIDTH)
                                    .label(i18n("shell-workspace-cancel")),
                            ),
                        ),
                )
                .content(move |content, _, _| {
                    content.child(
                        v_flex()
                            .gap_2()
                            .child(div().text_sm().child(i18n("shell-workspace-name-label")))
                            .child(Input::new(&content_name))
                            .child(content_dirs.clone()),
                    )
                })
                .on_ok(move |_, window, cx| {
                    let name = name_input.read(cx).value().trim().to_string();
                    let Some(roots) = dirs.read(cx).roots().cloned() else {
                        return false;
                    };

                    shell.update(cx, |this, cx| {
                        this.create_workspace(name, roots, window, cx)
                    });

                    true
                })
        });
    }

    /// Adopt a temporary workspace: from here on it is saved with the session
    /// like any other. Already-persistent workspaces are unaffected.
    pub(crate) fn activate_as_workspace(&mut self, id: WorkspaceId, cx: &mut Context<Self>) {
        self.workspaces.set_temporary(id, false);

        self.sync_session_memory(cx);

        cx.notify();
    }

    /// Create a workspace named `name` (empty falls back to the shared default)
    /// whose shells start in `dir` (empty falls back to the default
    /// startup directory), seeded with one fresh tab, and activate it.
    ///
    /// The workspace starts out temporary — opening a directory to run one
    /// command should not grow the saved session behind the user's back — and
    /// the sidebar's "activate" action is what makes it stick.
    pub(super) fn create_workspace(
        &mut self,
        name: String,
        roots: WorkspaceRoots,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = if name.is_empty() {
            i18n("shell-workspace-default-name").to_string()
        } else {
            name
        };

        let cwd = explicit_cwd(roots.primary());
        let surface_id = Self::alloc_id(&mut self.next_id);
        let default_profile = Self::default_profile(cx);
        let pane = spawn_default_pane(cx, surface_id, default_profile, cwd.clone());

        self.register_agent_pane(&pane, cx);

        let title = pane.read(cx).profile_name().to_string();

        let tabs = TabManager::new(
            TabSurface::Live(PaneTree::new_leaf(PaneId(surface_id), pane)),
            TabId(surface_id),
            title,
        );

        let ws_id = Self::alloc_id(&mut self.next_id);

        let ws_id = self
            .workspaces
            .new_workspace(tabs, WorkspaceId(ws_id), name, roots);

        self.workspaces.set_temporary(ws_id, true);

        self.focus_active(window, cx);

        self.refresh_root_availability(cx);
        self.sync_session_memory(cx);

        cx.notify();
    }

    pub(super) fn on_next_workspace(
        &mut self,
        _: &NextWorkspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let len = self.workspaces.len();
        let next = (self.workspaces.active_index() + 1) % len;

        self.workspaces.activate(next);

        self.focus_active(window, cx);

        self.sync_session_memory(cx);

        cx.notify();
    }

    pub(super) fn on_prev_workspace(
        &mut self,
        _: &PrevWorkspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let len = self.workspaces.len();
        let prev = (self.workspaces.active_index() + len - 1) % len;

        self.workspaces.activate(prev);

        self.focus_active(window, cx);

        self.sync_session_memory(cx);

        cx.notify();
    }
}

/// Walk from just after `active` and wrap around, returning the first marked
/// slot. `active` itself is visited last, so a tab that gets marked while it is
/// the one on screen stays reachable instead of being skipped forever.
pub(super) fn next_marked_position(marks: &[bool], active: usize) -> Option<usize> {
    (1..=marks.len())
        .map(|offset| (active + offset) % marks.len())
        .find(|&index| marks[index])
}
