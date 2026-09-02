//! Settings as a workspace of its own.
//!
//! Settings opens as a workspace rather than a dialog so the window keeps its
//! tabs while it is up, which means it also has to be retired again and the
//! previous workspace restored when the user leaves.

use gpui::{Context, Window};
use gpui_component::setting::{SelectIndex, SettingsState};
use nmt_i18n::i18n;

use crate::tabs::{TabId, TabManager};
use crate::ui;
use crate::ui::settings::AppSettings;
use crate::ui::shell::Shell;
use crate::ui::shell::actions::ShowSettings;
use crate::ui::shell::tab_surface::TabSurface;
use crate::workspace::{WorkspaceId, WorkspaceKind};

/// Sidebar entry name and tab title of the settings pseudo workspace, in the
/// active language. Looked up at creation time; the entry is never persisted,
/// so a stale-language name cannot leak into local_state.
pub(super) fn settings_title() -> &'static str {
    i18n("shell-workspace-settings-title")
}

impl Shell {
    /// Show settings as a pseudo workspace: a sidebar entry holding a single
    /// `Settings` tab whose surface fills the main area. A modal would block
    /// the terminal the user is adjusting settings for, while an entry can be
    /// left open and switched away from.
    ///
    /// Field edits mutate the `AppSettings` global live (for preview); the set
    /// is written when the entry closes and again on quit, since an entry the
    /// user never closes would otherwise never reach the file.
    pub(super) fn on_show_settings(
        &mut self,
        _: &ShowSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(id) = self.workspaces.settings_id() {
            if let Some(index) = self
                .workspaces
                .summaries()
                .iter()
                .position(|ws| ws.id == id)
            {
                self.workspaces.activate(index);
                self.focus_active(window, cx);
                cx.notify();
            }

            return;
        }

        // Live theme previews need the themes directory watched for as long as
        // the settings surface is on screen.
        self.theme_watcher = ui::watch_themes(cx);

        // Nothing else repaints on a page click, because the state lives
        // outside the element tree that would otherwise notify for it.
        let state = SettingsState::owned(SelectIndex::default(), window, cx);

        cx.observe(&state, |_, _, cx| cx.notify()).detach();

        self.settings_state = Some(state);

        let id = Self::alloc_id(&mut self.next_id);
        let tabs = TabManager::new(
            TabSurface::Settings,
            TabId(id),
            settings_title().to_string(),
        );
        let ws_id = Self::alloc_id(&mut self.next_id);

        self.workspaces.new_workspace_of_kind(
            tabs,
            WorkspaceId(ws_id),
            settings_title().to_string(),
            // The settings entry is a view of the configuration file, so it
            // owns no directory and never contributes to path routing.
            None,
            WorkspaceKind::Settings,
        );

        self.focus_active(window, cx);

        cx.notify();
    }

    /// Persist settings and drop the machinery the settings surface owns.
    /// Reached from every path that removes the settings entry.
    pub(super) fn retire_settings_workspace(&mut self, cx: &mut Context<Self>) {
        cx.global::<AppSettings>().save();
        // Pick up relay URL / token edits made while the entry was open.
        ui::settings::reconcile_remote_host(cx);

        // The surface is gone, so the next activation has nothing to flush.
        self.settings_was_active = false;

        self.theme_watcher = None;
        // Reopening starts on the first page, matching what the modal did.
        self.settings_state = None;
    }

    /// Leave the settings entry for a normal workspace. Every path that adds a
    /// tab funnels through this, so a new tab never lands in the settings
    /// entry and breaks its single-tab presentation.
    pub(crate) fn leave_settings_workspace(&mut self) {
        if self.workspaces.active_kind() == WorkspaceKind::Settings {
            let index = self.workspaces.first_normal_index();

            self.workspaces.activate(index);
        }
    }
}
