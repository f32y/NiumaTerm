use nmt_i18n::i18n;

use crate::ui::shell::*;

impl Shell {
    /// Build an inline-rename input pre-filled with `current`, focused with the
    /// current name selected, and configured so Enter or blur (clicking anywhere
    /// else) invokes `finish` with commit = true. Escape is intercepted by the
    /// hosting row, which calls `finish` with commit = false.
    fn rename_input(
        current: String,
        finish: fn(&mut Self, bool, &mut Window, &mut Context<Self>),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        let input = cx.new(|cx| InputState::new(window, cx).default_value(current));

        cx.subscribe_in(
            &input,
            window,
            move |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                    finish(this, true, window, cx);
                }
            },
        )
        .detach();

        input.update(cx, |input, cx| {
            input.focus(window, cx);
            input.set_selected_range(0..input.text().len(), cx);
        });

        input
    }

    /// Start renaming a workspace inline in the sidebar: the item swaps its
    /// name for an input pre-filled with the current name.
    pub(crate) fn start_workspace_rename(
        &mut self,
        id: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(current) = self
            .workspaces
            .summaries()
            .into_iter()
            .find(|ws| ws.id == id)
            .map(|ws| ws.name)
        else {
            return;
        };

        let input = Self::rename_input(current, Self::finish_workspace_rename, window, cx);

        self.workspace_rename = Some((id, input));

        cx.notify();
    }

    /// End the in-flight workspace rename. Enter and blur commit the entered
    /// name (blank names are dropped by the manager); Escape reaches this
    /// with `commit` false, keeping the original name.
    pub(in crate::ui) fn finish_workspace_rename(
        &mut self,
        commit: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((id, input)) = self.workspace_rename.take() else {
            return;
        };

        if commit {
            let name = input.read(cx).value().trim().to_string();

            self.workspaces.rename(id, name);

            self.sync_session_memory(cx);
        }

        self.focus_active(window, cx);
        cx.notify();
    }

    /// Start renaming a tab inline in the tab bar: the tab swaps its label
    /// for an input pre-filled with the current title.
    pub(crate) fn start_tab_rename(
        &mut self,
        id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(current) = self
            .workspaces
            .active_tabs()
            .find(id)
            .map(|tab| tab.title().to_string())
        else {
            return;
        };

        let input = Self::rename_input(current, Self::finish_tab_rename, window, cx);

        self.tab_rename = Some((id, input));

        cx.notify();
    }

    /// End the in-flight tab rename; same semantics as the workspace rename
    /// (Enter/blur commit, Escape cancels, blank names are dropped).
    pub(in crate::ui) fn finish_tab_rename(
        &mut self,
        commit: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((id, input)) = self.tab_rename.take() else {
            return;
        };

        if commit {
            let name = input.read(cx).value().trim().to_string();

            if !name.is_empty() {
                if let Some(tabs) = self.workspaces.tab_manager_for_mut(id) {
                    tabs.rename(id, name);

                    self.sync_session_memory(cx);
                }
            }
        }

        self.focus_active(window, cx);

        cx.notify();
    }

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

    /// Open the new-workspace dialog with the shared default name and a working
    /// directory. Confirming creates the workspace; cancel creates nothing.
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

        let dir_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(i18n("shell-workspace-dir-placeholder"))
        });

        let shell = cx.entity();

        window.open_dialog(cx, move |dialog, window, _| {
            let name_input = name_input.clone();
            let dir_input = dir_input.clone();
            let content_name = name_input.clone();
            let content_dir = dir_input.clone();
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
                            DialogClose::new().child(
                                Button::new("cancel-ws").label(i18n("shell-workspace-cancel")),
                            ),
                        )
                        .child(
                            DialogAction::new().child(
                                Button::new("create-ws")
                                    .label(i18n("shell-workspace-create"))
                                    .primary(),
                            ),
                        ),
                )
                .content(move |content, _, cx| {
                    let browse_dir = content_dir.clone();
                    content.child(
                        v_flex()
                            .gap_2()
                            .child(div().text_sm().child(i18n("shell-workspace-name-label")))
                            .child(Input::new(&content_name))
                            .child(div().text_sm().child(i18n("shell-workspace-dir-label")))
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(div().flex_1().child(Input::new(&content_dir)))
                                    .child(
                                        Button::new("browse-workspace-dir")
                                            .label(i18n("shell-workspace-browse"))
                                            .on_click(move |_, window, cx| {
                                                let rx = cx.prompt_for_paths(PathPromptOptions {
                                                    files: false,
                                                    directories: true,
                                                    multiple: false,
                                                    prompt: None,
                                                    file_types: Vec::new(),
                                                });

                                                let dir_input = browse_dir.clone();

                                                window
                                                    .spawn(cx, async move |cx| {
                                                        if let Ok(Ok(Some(paths))) = rx.await
                                                            && let Some(path) = paths.first()
                                                        {
                                                            let value = path.display().to_string();

                                                            let _ = dir_input.update_in(
                                                                cx,
                                                                |state, window, cx| {
                                                                    state.set_value(
                                                                        value, window, cx,
                                                                    )
                                                                },
                                                            );
                                                        }
                                                    })
                                                    .detach();
                                            }),
                                    ),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(i18n("shell-workspace-dir-description")),
                            ),
                    )
                })
                .on_ok(move |_, window, cx| {
                    let name = name_input.read(cx).value().trim().to_string();
                    let dir = dir_input.read(cx).value().trim().to_string();

                    if dir.is_empty() {
                        return false;
                    }

                    shell.update(cx, |this, cx| this.create_workspace(name, dir, window, cx));

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
        dir: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = if name.is_empty() {
            i18n("shell-workspace-default-name").to_string()
        } else {
            name
        };

        let cwd = explicit_cwd(&dir);
        let surface_id = Self::alloc_id(&mut self.next_id);
        let default_profile = Self::default_profile(cx);
        let pane = Self::spawn_default_pane(cx, surface_id, default_profile, cwd.clone());

        self.register_agent_pane(&pane, cx);

        let title = pane.read(cx).profile_name().to_string();

        let tabs = TabManager::new(
            TabSurface::Live(PaneTree::new_leaf(PaneId(surface_id), pane)),
            TabId(surface_id),
            title,
        );

        let ws_id = Self::alloc_id(&mut self.next_id);
        let ws_cwd = cwd.unwrap_or_else(|| ".".to_string());

        let ws_id = self
            .workspaces
            .new_workspace(tabs, WorkspaceId(ws_id), name, ws_cwd);

        self.workspaces.set_temporary(ws_id, true);

        self.focus_active(window, cx);

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
