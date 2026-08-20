use nmt_i18n::i18n;

use crate::agent::RecoveryIdentity;
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
        self.leave_settings_workspace();

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
            window.push_notification(i18n("shell-remote-no-hosts"), cx);
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
                        this.leave_settings_workspace();
                        this.register_agent_pane(&pane, cx);
                        this.workspaces.active_tabs_mut().new_tab(
                            TabSurface::Live(PaneTree::new_leaf(PaneId(id), pane)),
                            TabId(id),
                            i18n("shell-remote-tab-title").to_string(),
                        );
                        this.focus_active(window, cx);
                        cx.notify();
                    }
                    Err(e) => {
                        window.push_notification(
                            i18n("shell-remote-session-failed")
                                .replace("{error}", &e.to_string())
                                .as_str(),
                            cx,
                        );
                    }
                },
                Err(e) => {
                    window.push_notification(
                        i18n("shell-remote-connect-failed")
                            .replace("{error}", &e.to_string())
                            .as_str(),
                        cx,
                    );
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
        self.leave_settings_workspace();

        let cwd = explicit_cwd(self.workspaces.active_cwd());

        self.open_agent_tab_in(&profile, cwd, None, window, cx);
    }

    /// Open an agent tab rooted at `cwd`, optionally continuing `resume` once
    /// its session starts. A conversation belongs to the directory it ran in,
    /// so one listed from another tab opens here rather than in the tab that
    /// listed it.
    pub(super) fn open_agent_tab_in(
        &mut self,
        profile: &AgentProfile,
        cwd: Option<String>,
        resume: Option<RecoveryIdentity>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = Self::alloc_id(&mut self.next_id);

        // The tab is titled by the profile so multiple profiles of the same
        // agent stay distinguishable; an unnamed profile falls back to the
        // agent name.
        let title = if profile.name.trim().is_empty() {
            AgentKind::from_profile(profile.kind).display().to_string()
        } else {
            profile.name.clone()
        };

        let pane = cx.new(|cx| AgentPane::new_resuming(profile.clone(), cwd, resume, window, cx));

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

    /// CLI `new_tab`: reuse the workspace rooted exactly at `path`, otherwise
    /// open a fresh workspace there. With `open_in_best_workspace` on, a
    /// containing workspace is preferred over a new one and gets the tab
    /// instead, with the shell started in `path`.
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

        let containing = cx
            .global::<AppSettings>()
            .open_in_best_workspace
            .then(|| best_match(&self.workspaces.summaries(), path))
            .flatten();

        let Some(ws_id) = containing else {
            self.create_workspace(
                i18n("shell-workspace-default-name").into(),
                target,
                window,
                cx,
            );
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

        self.leave_settings_workspace();

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
