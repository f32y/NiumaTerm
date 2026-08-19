use std::time::Duration;
use std::{process, thread};

use gpui_component::StyledExt;
use nmt_i18n::i18n;

use crate::ui::shell::*;

pub(super) fn should_confirm_tab_close(
    is_agent: bool,
    confirm_agent_close: bool,
    warn: WarnBeforeTerminatingShell,
    child_process_count: usize,
) -> bool {
    should_confirm_close(is_agent && confirm_agent_close, warn, child_process_count)
}

pub(super) fn should_confirm_close(
    confirm: bool,
    warn: WarnBeforeTerminatingShell,
    child_process_count: usize,
) -> bool {
    confirm || warn.should_warn(child_process_count)
}

impl Shell {
    pub(super) fn on_close_tab(
        &mut self,
        _: &CloseTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A tab without panes (agent tab) has no pane-close cascade; it goes
        // straight to the tab-close path.
        let Some(tree) = self.workspaces.active_tabs().active().tree() else {
            let id = self.workspaces.active_tabs().active_id();
            self.request_close_tab(id, window, cx);
            return;
        };

        // With the tab split, the close shortcut closes the focused pane; the
        // last remaining pane falls through to the tab-close cascade.
        if !tree.is_single_leaf() {
            self.request_close_pane(window, cx);
            return;
        }

        let id = self.workspaces.active_tabs().active_id();

        self.request_close_tab(id, window, cx);
    }

    /// Close the focused pane of the active (multi-pane) tab, with a confirm
    /// dialog first when its shell has running child processes.
    fn request_close_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.workspaces.active_tabs().active().live().focused();
        let pane = self.active_pane();
        let settings = cx.global::<AppSettings>();
        let count = if settings.manage_subprocess_job
            && settings.warn_before_terminating_shell != WarnBeforeTerminatingShell::Disabled
        {
            pane.read(cx).child_process_count()
        } else {
            0
        };

        if !settings.warn_before_terminating_shell.should_warn(count) {
            self.close_pane_now(id, window, cx);
            return;
        }

        let description = if count > 0 {
            i18n("shell-close-pane-processes-description")
                .replace("{processes}", &Self::processes_running(count))
        } else {
            i18n("shell-close-pane-description").to_string()
        };

        Self::open_close_confirm(
            window,
            cx,
            i18n("shell-close-pane-title"),
            description,
            None,
            move |this, window, cx| this.close_pane_now(id, window, cx),
        );
    }

    fn close_pane_now(&mut self, id: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        let tree = self.workspaces.active_tabs_mut().active_mut().live_mut();

        let Some((pane, outcome)) = tree.remove(id) else {
            return;
        };

        if let RemoveOutcome::RemovedFromSplit { state, index } = outcome {
            state.update(cx, |state, cx| state.remove_panel(index, cx));
        }

        let route = pane.read(cx).agent_route().clone();

        self.remove_agent_route(&route, cx);

        // Dropping the pane entity drops its surface, releasing the IO thread
        // and ConPTY handle (same Drop chain as a tab close).
        drop(pane);

        self.focus_active(window, cx);
        self.sync_session_memory(cx);

        cx.notify();
    }

    /// "1 child process is running" / "N child processes are running" — the
    /// lead-in of every close-confirmation description.
    fn processes_running(count: usize) -> String {
        if count == 1 {
            i18n("shell-close-one-process-running").to_string()
        } else {
            i18n("shell-close-many-processes-running").replace("{count}", &count.to_string())
        }
    }

    /// "You have N temporary workspaces." for the dialogs that end this
    /// window, or `None` when every workspace here is saved. Temporary
    /// workspaces stay out of local_state, so they are the part of the window
    /// that will not come back.
    fn temporary_workspace_note(&self) -> Option<SharedString> {
        let count = self
            .workspaces
            .summaries()
            .iter()
            .filter(|ws| ws.temporary)
            .count();

        match count {
            0 => None,
            1 => Some(i18n("shell-close-one-temporary-workspace").into()),
            _ => Some(
                i18n("shell-close-many-temporary-workspaces")
                    .replace("{count}", &count.to_string())
                    .into(),
            ),
        }
    }

    /// Child processes running across every pane of workspace `id`.
    fn workspace_process_count(&self, id: WorkspaceId, cx: &App) -> usize {
        self.workspaces.tabs_of(id).map_or(0, |tabs| {
            tabs.tabs()
                .iter()
                .map(|tab| self.close_process_count(tab.surface(), cx))
                .sum()
        })
    }

    /// Shared scaffolding of every close-confirmation alert: title +
    /// description, OK runs `on_confirm` against this shell. `note` adds a
    /// bold line under the description for a consequence the description
    /// itself does not cover.
    fn open_close_confirm(
        window: &mut Window,
        cx: &mut Context<Self>,
        // The catalog outlives the app, so a key-literal lookup yields a
        // `'static` title without allocating per dialog.
        title: &'static str,
        description: String,
        note: Option<SharedString>,
        on_confirm: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let shell = cx.entity();
        let on_confirm = Rc::new(on_confirm);

        window.open_alert_dialog(cx, move |alert, _, _| {
            let shell = shell.clone();
            let on_confirm = Rc::clone(&on_confirm);

            alert
                .confirm()
                .title(title)
                .description(
                    v_flex()
                        .gap_1()
                        .child(description.clone())
                        .children(note.clone().map(|note| div().font_bold().child(note))),
                )
                .on_ok(move |_, window, cx| {
                    let on_confirm = Rc::clone(&on_confirm);
                    shell.update(cx, |this, cx| on_confirm(this, window, cx));
                    true
                })
        });
    }

    /// Child processes running across every pane of this tab, summed over
    /// each shell's Job Object. The count enriches warnings but is not needed
    /// by the `Always` mode.
    fn close_process_count(&self, tree: &TabSurface, cx: &App) -> usize {
        let settings = cx.global::<AppSettings>();

        if !settings.manage_subprocess_job
            || settings.warn_before_terminating_shell == WarnBeforeTerminatingShell::Disabled
        {
            return 0;
        }

        tree.leaves()
            .into_iter()
            .map(|(_, pane)| pane.read(cx).child_process_count())
            .sum()
    }

    /// Close the tab in the active workspace, with a confirm dialog first
    /// when the shell has running child processes. Closing the last tab
    /// closes its workspace too (confirmed first); on the last workspace
    /// that routes into the quit/replace/cancel dialog.
    pub(crate) fn request_close_tab(
        &mut self,
        id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The settings entry holds one tab that terminates nothing, so its
        // close control is the same gesture as dismissing the entry.
        if self
            .workspaces
            .active_tabs()
            .find(id)
            .is_some_and(|tab| tab.surface().is_settings())
        {
            let ws_id = self.workspaces.active_id();

            self.close_workspace_now(ws_id, window, cx);

            return;
        }

        let (count, is_agent) = self
            .workspaces
            .active_tabs()
            .find(id)
            .map_or((0, false), |tab| {
                let surface = tab.surface();
                (self.close_process_count(surface, cx), surface.is_agent())
            });

        if self.workspaces.active_tabs().len() == 1 {
            let ws_id = self.workspaces.active_id();

            if self.workspaces.real_len() == 1 {
                self.confirm_close_last_workspace(ws_id, window, cx);
                return;
            }

            let description = if count > 0 {
                i18n("shell-close-last-tab-processes-description")
                    .replace("{processes}", &Self::processes_running(count))
            } else if is_agent {
                i18n("shell-close-last-tab-agent-description").to_string()
            } else {
                i18n("shell-close-last-tab-description").to_string()
            };

            Self::open_close_confirm(
                window,
                cx,
                i18n("shell-close-last-tab-title"),
                description,
                None,
                move |this, window, cx| this.close_workspace_now(ws_id, window, cx),
            );

            return;
        }

        let settings = cx.global::<AppSettings>();
        let warn = settings.warn_before_terminating_shell;

        if !should_confirm_tab_close(is_agent, settings.confirm_before_closing, warn, count) {
            self.close_tab_now(id, window, cx);
            return;
        }

        let description = if is_agent {
            i18n("shell-close-tab-agent-description").to_string()
        } else if count > 0 {
            i18n("shell-close-tab-processes-description")
                .replace("{processes}", &Self::processes_running(count))
        } else {
            i18n("shell-close-tab-description").to_string()
        };

        Self::open_close_confirm(
            window,
            cx,
            i18n("shell-close-tab-title"),
            description,
            None,
            move |this, window, cx| this.close_tab_now(id, window, cx),
        );
    }

    fn close_tab_now(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        // `close` refuses the last tab and returns the removed pane entity;
        // dropping it drops the pane's surface and PTY.
        if let Some(tree) = self.workspaces.active_tabs_mut().close(id) {
            for route in Self::agent_routes_in_surface(&tree, cx) {
                self.remove_agent_route(&route, cx);
            }

            drop(tree);

            self.focus_active(window, cx);
            self.sync_session_memory(cx);

            cx.notify();
        }
    }

    /// Close the workspace, with a confirm dialog first when the
    /// confirm-before-closing-workspace setting is on, or (with it off) when
    /// any of its shells has running child processes.
    pub(crate) fn request_close_workspace(
        &mut self,
        id: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.workspaces.is_pinned(id) {
            return;
        }

        // Dismissing the settings entry ends nothing the user could lose, so
        // it closes on the first click whatever the confirmation settings say.
        if self.workspaces.kind_of(id) == Some(WorkspaceKind::Settings) {
            self.close_workspace_now(id, window, cx);
            return;
        }

        if self.workspaces.real_len() == 1 {
            self.confirm_close_last_workspace(id, window, cx);
            return;
        }

        let confirm = cx.global::<AppSettings>().confirm_before_closing;

        let count = self.workspace_process_count(id, cx);

        let warn = cx.global::<AppSettings>().warn_before_terminating_shell;

        if !confirm && !warn.should_warn(count) {
            self.close_workspace_now(id, window, cx);
            return;
        }

        let description = if count > 0 {
            i18n("shell-close-workspace-processes-description")
                .replace("{processes}", &Self::processes_running(count))
        } else {
            i18n("shell-close-workspace-description").to_string()
        };

        Self::open_close_confirm(
            window,
            cx,
            i18n("shell-close-workspace-title"),
            description,
            None,
            move |this, window, cx| this.close_workspace_now(id, window, cx),
        );
    }

    /// Closing the last workspace is a three-way choice: quit the app (the
    /// workspace is then dropped from local_state, since the user asked to
    /// close it), swap in a fresh home-directory workspace, or cancel (a
    /// no-op: the workspace stays open and persists normally).
    fn confirm_close_last_workspace(
        &mut self,
        id: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let count = self.workspace_process_count(id, cx);

        let message = if count > 0 {
            i18n("shell-close-last-workspace-processes-message")
                .replace("{processes}", &Self::processes_running(count))
        } else {
            i18n("shell-close-last-workspace-message").to_string()
        };

        // Quitting from here saves the session, so the same warning the
        // window-close dialog carries applies to this choice too.
        let note = self.temporary_workspace_note();

        let shell = cx.entity();

        window.open_dialog(cx, move |dialog, _, _| {
            let quit_shell = shell.clone();
            let replace_shell = shell.clone();
            let message = message.clone();
            let note = note.clone();

            dialog
                .title(i18n("shell-close-last-workspace-title"))
                .overlay_closable(false)
                .content(move |content, _, cx| {
                    content.child(
                        v_flex()
                            .gap_1()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(message.clone())
                            .children(note.clone().map(|note| div().font_bold().child(note))),
                    )
                })
                .footer(
                    DialogFooter::new()
                        .child(
                            DialogClose::new()
                                .child(Button::new("keep-ws").label(i18n("shell-close-cancel"))),
                        )
                        .child(
                            Button::new("replace-ws")
                                .label(i18n("shell-close-new-default-workspace"))
                                .primary()
                                .on_click(move |_, window, cx| {
                                    window.close_dialog(cx);
                                    replace_shell.update(cx, |this, cx| {
                                        this.replace_last_workspace(id, window, cx)
                                    });
                                }),
                        )
                        .child(
                            Button::new("quit-app")
                                .label(i18n("shell-close-quit"))
                                .danger()
                                .on_click(move |_, _, cx| {
                                    quit_shell.update(cx, |this, cx| this.doom_workspace(id, cx));
                                    cx.quit();
                                }),
                        ),
                )
        });
    }

    /// Exclude `id` from session persistence and push the trimmed session to
    /// the registry so the quit hook saves local_state without it.
    fn doom_workspace(&mut self, id: WorkspaceId, cx: &mut Context<Self>) {
        self.doomed_workspace = Some(id);

        self.sync_session_memory(cx);
    }

    /// Swap the last workspace for a fresh default one rooted in the user's
    /// home directory, then close the old one (creation first, so the
    /// never-empty invariant of `WorkspaceManager` holds throughout).
    fn replace_last_workspace(
        &mut self,
        id: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let home = home_dir()
            .map(|home| home.display().to_string())
            .unwrap_or_default();

        self.create_workspace(String::new(), home, window, cx);

        self.close_workspace_now(id, window, cx);
    }

    /// True when the window may close right away. The explicit confirmation
    /// setting and terminal child-process warnings share this path. Reached
    /// from the titlebar X and the OS close request (Alt+F4, taskbar).
    pub(crate) fn confirm_window_close(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let count: usize = self
            .workspaces
            .all_tabs()
            .flat_map(|tabs| tabs.tabs())
            .map(|tab| self.close_process_count(tab.surface(), cx))
            .sum();

        let settings = cx.global::<AppSettings>();
        let warn = settings.warn_before_terminating_shell;

        if !should_confirm_close(settings.confirm_before_closing, warn, count) {
            return true;
        }

        let description = if count > 0 {
            i18n("shell-close-window-processes-description")
                .replace("{processes}", &Self::processes_running(count))
        } else {
            i18n("shell-close-window-description").to_string()
        };

        let note = self.temporary_workspace_note();

        // `remove_window` tears the window down directly (no WM_CLOSE
        // round-trip), so this dialog won't re-trigger.
        Self::open_close_confirm(
            window,
            cx,
            i18n("shell-close-window-title"),
            description,
            note,
            |_, window, cx| begin_window_teardown(window, cx),
        );
        false
    }

    fn close_workspace_now(
        &mut self,
        id: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let routes = self
            .workspaces
            .tabs_of(id)
            .map(|tabs| {
                tabs.tabs()
                    .iter()
                    .flat_map(|tab| Self::agent_routes_in_surface(tab.surface(), cx))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let settings = self.workspaces.kind_of(id) == Some(WorkspaceKind::Settings);

        if self.workspaces.close_workspace(id).is_some() {
            for route in routes {
                self.remove_agent_route(&route, cx);
            }

            if settings {
                self.retire_settings_workspace(cx);
            }

            self.focus_active(window, cx);

            self.sync_session_memory(cx);

            cx.notify();
        }
    }
}

/// How long the process may spend releasing a hidden window before it is cut
/// short. The window is already off the screen by then, so a teardown that
/// stalls leaves no way to tell the application is still running: an
/// unbounded wait is an invisible process the user can only reach through the
/// task manager.
const TEARDOWN_DEADLINE: Duration = Duration::from_secs(5);

/// Take the window off the screen and start the clock on releasing it.
///
/// Closing tears the window down synchronously once the update returns, and
/// that teardown kills whole process trees: every shell's job object, every
/// agent's CLI. Hiding first is what makes the click feel like a close, and
/// the release then runs with nothing on screen.
pub(super) fn begin_window_teardown(window: &mut Window, cx: &mut App) {
    // The deadline ends the process, so it belongs only to the window whose
    // closing ends it. Closing one of several leaves the application running,
    // and its teardown is bounded by the windows that outlive it.
    let last_window = cx.windows().len() <= 1;

    window.hide_window();
    window.remove_window();

    if !last_window {
        return;
    }

    // A plain OS thread, because the executor this would otherwise run on is
    // part of what is being torn down. Exiting zero: whatever has not finished
    // by now is release work, and the state files are written by the quit hook
    // that runs well before this deadline.
    thread::spawn(|| {
        thread::sleep(TEARDOWN_DEADLINE);
        warn!("window teardown exceeded {TEARDOWN_DEADLINE:?}; exiting");
        process::exit(0);
    });
}
