//! Inline renaming of a workspace entry or a tab.
//!
//! Both renames replace a label with a text input in place rather than opening
//! a dialog, and both commit on Enter or blur and cancel on Escape. At most one
//! of each can be in flight, so the session below is what every row asks
//! whether it is the one being renamed.

use gpui::prelude::*;
use gpui::{Context, Entity, Window};
use gpui_component::input::{InputEvent, InputState};

use crate::tabs::TabId;
use crate::ui::shell::Shell;
use crate::workspace::WorkspaceId;

/// The in-flight inline renames. A rename is identified by the row it belongs
/// to, so a row can ask for its own input without any other row matching.
#[derive(Default)]
pub(crate) struct InlineRenameSession {
    workspace: Option<(WorkspaceId, Entity<InputState>)>,
    tab: Option<(TabId, Entity<InputState>)>,
}

impl InlineRenameSession {
    pub(super) fn begin_workspace(
        &mut self,
        id: WorkspaceId,
        current: String,
        window: &mut Window,
        cx: &mut Context<Shell>,
    ) {
        let input = rename_input(current, Shell::finish_workspace_rename, window, cx);

        self.workspace = Some((id, input));
    }

    pub(super) fn begin_tab(
        &mut self,
        id: TabId,
        current: String,
        window: &mut Window,
        cx: &mut Context<Shell>,
    ) {
        let input = rename_input(current, Shell::finish_tab_rename, window, cx);

        self.tab = Some((id, input));
    }

    pub(super) fn take_workspace(&mut self) -> Option<(WorkspaceId, Entity<InputState>)> {
        self.workspace.take()
    }

    pub(super) fn take_tab(&mut self) -> Option<(TabId, Entity<InputState>)> {
        self.tab.take()
    }

    /// The input this workspace row should draw in place of its name, if it is
    /// the one being renamed.
    pub(crate) fn workspace_input(&self, id: WorkspaceId) -> Option<&Entity<InputState>> {
        self.workspace
            .as_ref()
            .filter(|(renaming, _)| *renaming == id)
            .map(|(_, input)| input)
    }

    /// The input this tab should draw in place of its label, if it is the one
    /// being renamed.
    pub(crate) fn tab_input(&self, id: TabId) -> Option<&Entity<InputState>> {
        self.tab
            .as_ref()
            .filter(|(renaming, _)| *renaming == id)
            .map(|(_, input)| input)
    }
}

/// Build an inline-rename input pre-filled with `current`, focused with the
/// current name selected, and configured so Enter or blur (clicking anywhere
/// else) invokes `finish` with commit = true. Escape is intercepted by the
/// hosting row, which calls `finish` with commit = false.
fn rename_input(
    current: String,
    finish: fn(&mut Shell, bool, &mut Window, &mut Context<Shell>),
    window: &mut Window,
    cx: &mut Context<Shell>,
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

impl Shell {
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

        self.renames.begin_workspace(id, current, window, cx);

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
        let Some((id, input)) = self.renames.take_workspace() else {
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

        self.renames.begin_tab(id, current, window, cx);

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
        let Some((id, input)) = self.renames.take_tab() else {
            return;
        };

        if commit {
            let name = input.read(cx).value().trim().to_string();

            if !name.is_empty() {
                let mut renamed_agent = None;

                if let Some(tabs) = self.workspaces.tab_manager_for_mut(id) {
                    tabs.rename(id, name.clone());
                    renamed_agent = tabs.find(id).and_then(|tab| tab.surface().agent().cloned());

                    self.sync_session_memory(cx);
                }

                // An agent tab's name is the conversation's name, so it goes
                // to the harness too: its session listing is what this
                // application's own recent-sessions list reads.
                if let Some(agent) = renamed_agent {
                    agent.update(cx, |agent, _| agent.rename_session(&name));
                }
            }
        }

        self.focus_active(window, cx);

        cx.notify();
    }
}
