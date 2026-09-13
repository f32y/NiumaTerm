use app::agent_tab::RecoveryIdentity;
use app::agent_tab::execution::AgentSession;
use app::agent_tab::team::{TeamPane, TeamRuntime};
use nmt_agent::team::identity::RoomId;
use nmt_config::config_dir_path;
use nmt_config::local_state::TabState;
use rust_i18n::t;

use crate::ui::persistence::spawn_default_pane;
use crate::ui::shell::actions::NewTeamTab;
use crate::ui::shell::tab_surface::AgentTab;
use crate::ui::shell::*;
use crate::ui::terminal_launch::attach_remote;

impl Shell {
    fn insert_tab(
        &mut self,
        id: TabId,
        surface: TabSurface,
        title: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.workspaces
            .active_tabs_mut()
            .new_tab(surface, id, title);

        self.on_active_tab_changed(window, cx);
        self.focus_active(window, cx);
        self.sync_session_memory(cx);

        cx.notify();
    }

    pub(crate) fn open_team_tab(
        &mut self,
        saved: Option<RoomId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.leave_settings_workspace();

        let directory = config_dir_path();

        let runtime = match saved {
            Some(id) => TeamRuntime::open(&directory, id, cx),

            None => TeamRuntime::create(
                &directory,
                agent_workspace(self.workspaces.active_roots()),
                cx,
            ),
        };

        let surface = match runtime {
            Ok(runtime) => TabSurface::Team(cx.new(|cx| TeamPane::new(runtime, window, cx))),

            Err(error) => TabSurface::TeamUnavailable {
                saved: Box::new(TabState {
                    team_room: saved.map(|id| id.to_string()),
                    ..TabState::default()
                }),
                message: error.to_string(),
            },
        };

        let id = Self::alloc_id(&mut self.next_id);

        self.insert_tab(
            TabId(id),
            surface,
            t!("team-title").into_owned(),
            window,
            cx,
        );
    }

    pub(crate) fn on_new_team_tab(
        &mut self,
        _: &NewTeamTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_team_tab(None, window, cx);
    }

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
    /// from the new-tab menu, or the default profile) in the active
    /// workspace's primary directory.
    pub(crate) fn open_profile_tab(
        &mut self,
        profile: (Option<String>, Vec<String>),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // `leave_settings_workspace` can change which workspace is active, so
        // the primary directory is read only after the move.
        self.leave_settings_workspace();

        let cwd = self.workspaces.active_cwd().to_string();

        self.open_profile_tab_in_directory(profile, cwd, window, cx);
    }

    /// Open a terminal tab running `profile` with `cwd` as the process working
    /// directory. Starting a process somewhere is not the same as the
    /// workspace living there, so this leaves the workspace's own directories
    /// untouched.
    pub(crate) fn open_profile_tab_in_directory(
        &mut self,
        profile: (Option<String>, Vec<String>),
        cwd: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.leave_settings_workspace();

        let id = Self::alloc_id(&mut self.next_id);

        let pane = spawn_default_pane(cx, id, profile, explicit_cwd(&cwd));

        self.register_agent_pane(&pane, cx);

        let title = pane.read(cx).profile_name().to_string();

        self.insert_tab(
            TabId(id),
            TabSurface::Live(TerminalLayout::new_leaf(PaneId(id), pane)),
            title,
            window,
            cx,
        );
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
            window.push_notification(t!("shell-remote-no-hosts"), cx);

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
                Ok(remote) => match attach_remote(cx, id, remote) {
                    Ok(pane) => {
                        this.leave_settings_workspace();
                        this.register_agent_pane(&pane, cx);

                        this.insert_tab(
                            TabId(id),
                            TabSurface::Live(TerminalLayout::new_leaf(PaneId(id), pane)),
                            t!("shell-remote-tab-title").to_string(),
                            window,
                            cx,
                        );
                    }

                    Err(e) => {
                        window.push_notification(
                            t!("shell-remote-session-failed", error = e)
                                .into_owned()
                                .as_str(),
                            cx,
                        );
                    }
                },

                Err(e) => {
                    window.push_notification(
                        t!("shell-remote-connect-failed", error = e)
                            .into_owned()
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

        let workspace = agent_workspace(self.workspaces.active_roots());

        self.open_agent_tab_in(&profile, workspace, None, window, cx);
    }

    /// Open an agent tab rooted at `cwd`, optionally continuing `resume` once
    /// its session starts. A conversation belongs to the directory it ran in,
    /// so one listed from another tab opens here rather than in the tab that
    /// listed it.
    pub(super) fn open_agent_tab_in(
        &mut self,
        profile: &AgentProfile,
        workspace: AgentWorkspace,
        resume: Option<RecoveryIdentity>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = Self::alloc_id(&mut self.next_id);

        // The tab is titled by the profile so multiple profiles of the same
        // agent stay distinguishable; an unnamed profile falls back to the
        // agent name.
        let title = if profile.name.trim().is_empty() {
            profile.kind.display().to_string()
        } else {
            profile.name.clone()
        };

        let owner = AgentSession::create(profile.clone(), workspace, None, cx);
        let pane = cx.new(|cx| AgentPane::attach(&owner, window, cx));

        Self::watch_agent_tab(&pane, cx);
        self.register_agent_tab(&pane, cx);

        owner.start(resume, cx);

        self.insert_tab(
            TabId(id),
            TabSurface::Agent(AgentTab { owner, pane }),
            title,
            window,
            cx,
        );
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
            self.on_active_tab_changed(window, cx);
            self.focus_active(window, cx);
            self.sync_session_memory(cx);

            cx.notify();

            return;
        }

        let target = path.display().to_string();

        let containing = cx
            .global::<AppSettings>()
            .config()
            .system
            .open_in_best_workspace
            .then(|| best_match(&self.workspaces.summaries(), path))
            .flatten();

        let Some(ws_id) = containing else {
            self.create_temporary_workspace(
                t!("shell-workspace-default-name").into(),
                WorkspaceRoots::single(target),
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

        let default_profile = Self::default_profile(cx);

        self.open_profile_tab_in_directory(default_profile, target, window, cx);
    }
}
