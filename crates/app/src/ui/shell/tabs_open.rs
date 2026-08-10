use crate::ui::shell::*;

impl Shell {
    pub(super) fn default_profile(cx: &Context<Self>) -> (Option<String>, Vec<String>) {
        cx.global::<AppSettings>().default_profile_command()
    }

    /// Open a new window with a fresh default session, offset from this one so
    /// the two don't exactly overlap.
    pub(super) fn on_new_window(
        &mut self,
        _: &NewWindow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let bounds = window.window_bounds().get_bounds();

        AppWindow::open(
            cx,
            AppWindow {
                bounds: Some(WindowState {
                    x: bounds.origin.x.as_f32() + 30.0,
                    y: bounds.origin.y.as_f32() + 30.0,
                    width: bounds.size.width.as_f32(),
                    height: bounds.size.height.as_f32(),
                    maximized: false,
                }),
                session: None,
                // New windows inherit this window's sidebar width.
                sidebar_width: Some(self.sidebar.width),
                initial_cwd: None,
            },
        );
    }

    pub(crate) fn on_new_tab(&mut self, _: &NewTab, window: &mut Window, cx: &mut Context<Self>) {
        let default_profile = Self::default_profile(cx);

        self.open_profile_tab(default_profile, window, cx);
    }

    /// Open a terminal tab running the given launch command (a profile picked
    /// from the new-tab menu, or the default profile).
    pub(crate) fn open_profile_tab(
        &mut self,
        profile: (Option<String>, Vec<String>),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = Self::alloc_id(&mut self.next_id);

        let cwd = explicit_cwd(self.workspaces.active_cwd());
        let pane = Self::spawn_default_pane(cx, id, profile, cwd);

        self.register_agent_pane(&pane, cx);

        let title = pane.read(cx).profile_name().to_string();

        self.workspaces.active_tabs_mut().new_tab(
            TabSurface::Live(PaneTree::new_leaf(PaneId(id), pane)),
            TabId(id),
            title,
        );

        self.focus_active(window, cx);

        self.sync_session_memory(cx);

        cx.notify();
    }

    /// Open a remote-session tab: connect to a paired host in the background,
    /// then add a tab whose terminal is fed over the network by `NetPty`.
    #[cfg(windows)]
    pub(crate) fn on_new_remote_tab(
        &mut self,
        _: &NewRemoteTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let hosts = remote::known_hosts();
        let Some(host) = hosts.into_iter().next() else {
            window.push_notification(
                "No paired remote hosts. Pair one in Settings → Remote Session.",
                cx,
            );
            return;
        };
        // Connects to the first paired host; a host picker is only meaningful
        // once a user keeps several hosts paired at the same time.
        let id = Self::alloc_id(&mut self.next_id);

        cx.spawn_in(window, async move |this, cx| {
            let connected = cx
                .background_executor()
                .spawn(async move { remote::connect_new_session(&host) })
                .await;

            let _ = this.update_in(cx, |this, window, cx| match connected {
                Ok(remote) => match TerminalPane::spawn_remote(cx, id, remote) {
                    Ok(pane) => {
                        this.register_agent_pane(&pane, cx);
                        this.workspaces.active_tabs_mut().new_tab(
                            TabSurface::Live(PaneTree::new_leaf(PaneId(id), pane)),
                            TabId(id),
                            "Remote".to_string(),
                        );
                        this.focus_active(window, cx);
                        cx.notify();
                    }
                    Err(e) => {
                        window
                            .push_notification(format!("Remote session failed: {e}").as_str(), cx);
                    }
                },
                Err(e) => {
                    window.push_notification(format!("Connect failed: {e}").as_str(), cx);
                }
            });
        })
        .detach();
    }

    /// Open an agent tab: an agent chat conversation in place of a terminal.
    /// The conversation's agent process starts in the workspace cwd.
    pub(crate) fn open_agent_tab(
        &mut self,
        profile: AgentProfile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = Self::alloc_id(&mut self.next_id);
        let cwd = explicit_cwd(self.workspaces.active_cwd());

        // The tab is titled by the profile so multiple profiles of the same
        // agent stay distinguishable; an unnamed profile falls back to the
        // agent name.
        let title = if profile.name.trim().is_empty() {
            AgentKind::from_profile(profile.kind).display().to_string()
        } else {
            profile.name.clone()
        };

        let pane = cx.new(|cx| AgentPane::new(profile, cwd, window, cx));

        Self::watch_agent_tab(&pane, cx);
        self.register_agent_tab(&pane, cx);

        self.workspaces
            .active_tabs_mut()
            .new_tab(TabSurface::Agent(pane), TabId(id), title);

        self.focus_active(window, cx);
        self.sync_session_memory(cx);

        cx.notify();
    }

    pub(crate) fn on_new_agent_tab(
        &mut self,
        _: &NewAgentTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let profile = cx.global::<AppSettings>().default_agent_profile_entry();
        self.open_agent_tab(profile, window, cx);
    }

    /// CLI `new_tab`: open `path` in the deepest workspace whose cwd contains
    /// it (new tab, shell starts in `path`), or in a fresh workspace when
    /// nothing matches, preserving the user's target.
    pub(crate) fn open_dir_tab(
        &mut self,
        path: &path::Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let summaries = self.workspaces.summaries();
        if let Some(index) = exact_match(&summaries, path).and_then(|workspace_id| {
            summaries
                .iter()
                .position(|workspace| workspace.id == workspace_id)
        }) {
            // Workspace activation preserves its TabManager's active index,
            // restoring the tab the user last used without spawning a shell.
            self.workspaces.activate(index);
            window.activate_window();
            self.focus_active(window, cx);
            self.sync_session_memory(cx);
            cx.notify();
            return;
        }

        let target = path.display().to_string();

        let Some(ws_id) = best_match(&self.workspaces.summaries(), path) else {
            self.create_workspace(DEFAULT_WORKSPACE_NAME.into(), target, window, cx);
            return;
        };

        if let Some(index) = self
            .workspaces
            .summaries()
            .iter()
            .position(|ws| ws.id == ws_id)
        {
            self.workspaces.activate(index);
        }

        let id = Self::alloc_id(&mut self.next_id);
        let default_profile = Self::default_profile(cx);
        let pane = Self::spawn_default_pane(cx, id, default_profile, Some(target));

        self.register_agent_pane(&pane, cx);

        let title = pane.read(cx).profile_name().to_string();

        self.workspaces.active_tabs_mut().new_tab(
            TabSurface::Live(PaneTree::new_leaf(PaneId(id), pane)),
            TabId(id),
            title,
        );

        self.focus_active(window, cx);
        self.sync_session_memory(cx);

        cx.notify();
    }
}
