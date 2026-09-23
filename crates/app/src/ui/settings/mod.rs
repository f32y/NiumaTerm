//! Persisted to `config.toml`: seeded via [`AppSettings::load`] at startup,
//! written back patch-style via [`AppSettings::save`] when the settings
//! workspace closes and on quit. Field edits mutate the global live for preview;
//! failed writes retain those edits and expose a retry action.
//!
//! The settings workspace owns its page state and theme watcher until it closes,
//! when the window returns to the previously active workspace.

pub use nmt_config::appearance::MAX_TAB_WIDTH;

pub use crate::ui::settings::state::{
    AgentProfile, AgentProfileLauncher, AppSettings, CollapseRows, EnvVar, InputStyle,
    MIN_TAB_WIDTH, ModelListStyle, Profile, SettingsEditing, TabBarStyle, WindowBackdrop,
};

pub(crate) use crate::ui::settings::opacity::{
    background_image_layer_opacity, main_view_background_opacity, window_background_appearance,
};
pub(crate) use crate::ui::settings::state::builtin_agent_profile;
#[cfg(test)]
pub(crate) use crate::ui::settings::state::{
    DEFAULT_FONT_FAMILY, DEFAULT_FONT_SIZE, DEFAULT_LINE_HEIGHT, DEFAULT_UI_FONT,
};
pub(crate) use crate::ui::settings::terminal_bridge::{
    install_agent_settings, install_terminal_settings,
};
pub(crate) use crate::ui::settings::theme::{apply_ui_theme, apply_window_translucency};

mod about_page;
mod agent_page;
mod agent_profile_page;
mod appearance_page;
mod card;
mod fields;
mod hooks;
#[cfg(target_os = "macos")]
mod macos_page;
mod opacity;
mod profiles_page;
mod state;
mod system_page;
mod table;
mod terminal_bridge;
mod terminal_page;
mod theme;
mod theme_gallery;

#[cfg(test)]
mod tests;

use std::borrow::Cow;
use std::io;
use std::path::PathBuf;

use app::design::SETTINGS_NAV_WIDTH;
use app::utils::background_write_reply;
#[cfg(test)]
use gpui::WindowBackgroundAppearance;
use gpui::{
    App, AppContext as _, Context, Entity, FileDialogFilter, IntoElement as _, ParentElement as _,
    PathPromptOptions, SharedString, Styled as _, Task, Window, relative,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::dialog::{DIALOG_BUTTON_MIN_WIDTH, DialogClose, DialogFooter};
use gpui_component::group_box::{GroupBox, GroupBoxVariants as _};
use gpui_component::input::{Input, InputEvent};
use gpui_component::label::Label;
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::notification::{Notification, NotificationType};
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::setting::{
    NumberFieldOptions, SelectIndex, SettingField, SettingGroup, SettingItem, SettingPage,
    Settings, SettingsState, SettingsView,
};
use gpui_component::switch::Switch;
use gpui_component::{
    ActiveTheme as _, Disableable as _, Sizable as _, Theme, WindowExt as _, h_flex, v_flex,
};
use nmt_agent::HookInstallStatus;
use nmt_agent::update::{DiscoverySupport, InstallationKey, ProviderKind, UpdatePhase};
#[cfg(test)]
use nmt_config::CursorShape;
use nmt_config::config_file_path;
use nmt_config::system::{NewlineShortcut, WarnBeforeTerminatingShell};
#[cfg(windows)]
use nmt_platform::{
    is_shell_integration_registered, register_shell_integration, set_system_notification_enabled,
    shell_integration_dll_mismatched, system_notification_enabled, unregister_shell_integration,
};
use rust_i18n::t;
use tracing::warn;

#[cfg(windows)]
use crate::PlatformHandle;
use crate::agent_updates::AgentUpdates;
use crate::ui::composition::sidebar_surface;
use crate::ui::settings::about_page::about_page;
use crate::ui::settings::agent_page::agent_page;
#[cfg(test)]
use crate::ui::settings::agent_page::{installation_update_title, installation_version_text};
use crate::ui::settings::agent_profile_page::{agent_profile_list, open_agent_profile_dialog};
use crate::ui::settings::appearance_page::appearance_page;
use crate::ui::settings::card::{card_row, card_text_input, description_hint};
use crate::ui::settings::fields::{
    background_image_field, background_image_opacity_field, background_opacity_field,
    tab_shape_field,
};
use crate::ui::settings::hooks::{AgentHooks, Hook};
#[cfg(test)]
use crate::ui::settings::opacity::{
    effective_background_image_layer_opacity, effective_background_opacity,
    effective_surface_background_opacity, window_background_appearance_for,
};
use crate::ui::settings::profiles_page::profiles_page;
#[cfg(test)]
use crate::ui::settings::state::{
    DEFAULT_AGENT_TRANSCRIPT_FONT_SIZE, DEFAULT_BACKGROUND_IMAGE_OPACITY, DEFAULT_TAB_WIDTH,
    clamp_agent_transcript_font_size, clamp_background_image_opacity, clamp_background_opacity,
    clamp_git_interval, clamp_tab_width, clamp_terminal_font_size, clamp_terminal_line_height,
    terminal_font_or_default, ui_font_or_default,
};
use crate::ui::settings::state::{agent_kind_display_label, input_style_label};
use crate::ui::settings::system_page::system_page;
use crate::ui::settings::table::{ENV_OPERATION_COLUMN, table_row};
use crate::ui::settings::terminal_page::terminal_page;
use crate::ui::settings::theme::watch_themes;
use crate::ui::settings::theme_gallery::theme_list;
use crate::ui::shell::AppWindow;
use crate::{agent_updates, ui};

const APP_VERSION: &str = env!("NIUMATERM_VERSION");
const APP_INTERNAL_VERSION: &str = env!("NIUMATERM_INTERNAL_VERSION");
const RELEASE_PAGE_URL: &str = "https://github.com/f32y/NiumaTerm/releases";

/// Sidebar entry name and tab title of the settings pseudo workspace, in the
/// active language. Looked up at creation time; the entry is never persisted,
/// so a stale-language name cannot leak into local_state.
pub(super) fn settings_title() -> Cow<'static, str> {
    t!("shell-workspace-settings-title")
}

/// Everything the settings surface owns while it is on screen: the page and
/// search state that outlives a repaint, the themes-directory watcher that
/// makes theme edits preview live.
#[derive(Default)]
pub(super) struct SettingsSurface {
    open: Option<OpenSettings>,
}

struct OpenSettings {
    view: Entity<SettingsView>,
    _theme_watcher: Option<Task<()>>,
}

impl SettingsSurface {
    pub(super) fn open(&mut self, window: &mut Window, cx: &mut Context<AppWindow>) {
        Hook::Claude.refresh(cx);
        Hook::Codex.refresh(cx);

        let state = SettingsState::owned(SelectIndex::default(), window, cx);
        let editing = cx.new(|_| SettingsEditing::default());

        let theme_watcher = watch_themes(&editing, cx);
        let view = new_settings_view(state, editing, cx);

        self.open = Some(OpenSettings {
            view,
            _theme_watcher: theme_watcher,
        });
    }

    pub(super) fn retire(&mut self) {
        self.open = None;
    }

    pub(super) fn render(&self, _: &App) -> Option<Entity<SettingsView>> {
        let open = self.open.as_ref()?;

        Some(open.view.clone())
    }
}

fn new_settings_view(
    state: Entity<SettingsState>,
    editing: Entity<SettingsEditing>,
    cx: &mut App,
) -> Entity<SettingsView> {
    cx.new(|cx| {
        cx.observe(&editing, |view: &mut SettingsView, _, cx| view.refresh(cx))
            .detach();

        cx.observe_global::<AppSettings>(|view, cx| view.refresh(cx))
            .detach();

        cx.observe_global::<AgentUpdates>(|view, cx| view.refresh(cx))
            .detach();

        cx.observe_global::<AgentHooks>(|view, cx| view.refresh(cx))
            .detach();

        cx.observe_global::<Theme>(|view, cx| view.refresh(cx))
            .detach();

        SettingsView::new(state, move |cx| settings_view(editing.clone(), cx), cx)
    })
}

struct SettingsSaveFailure;

/// Write the settings out and resolve with whether they reached disk. Failed
/// edits stay in memory, and the failure notification offers another write
/// after the user fixes the configuration file or its permissions.
pub(crate) fn save_settings(window: &mut Window, cx: &mut App) -> Task<bool> {
    save_settings_to(config_file_path(), window, cx)
}

fn save_settings_to(path: PathBuf, window: &mut Window, cx: &mut App) -> Task<bool> {
    let settings = cx.global::<AppSettings>().clone();
    let config = settings.config().clone();
    let target = path.clone();
    let saved = background_write_reply(cx, move || settings.save_to(&target));

    window.spawn(cx, async move |cx| {
        let result = saved.await;

        let completed = cx.update(|window, cx| {
            if result.is_ok() {
                cx.global_mut::<AppSettings>()
                    .mark_persisted(config.clone());
            }

            // Edits made while the write ran are still only in memory. Write
            // again before reporting, so a close waiting on this sees them.
            if result.is_ok() && *cx.global::<AppSettings>().config() != config {
                return save_settings_to(path, window, cx);
            }

            Task::ready(settings_save_completed(result, window, cx))
        });

        match completed {
            Ok(completed) => completed.await,
            Err(_) => false,
        }
    })
}

fn settings_save_completed(result: io::Result<()>, window: &mut Window, cx: &mut App) -> bool {
    match result {
        Ok(()) => {
            window.remove_notification::<SettingsSaveFailure>(cx);

            true
        }
        Err(error) => {
            warn!("failed to save settings: {error}");

            window.push_notification(
                Notification::new()
                    .id::<SettingsSaveFailure>()
                    .with_type(NotificationType::Error)
                    .title(t!("settings-save-failed-title"))
                    .message(format!(
                        "{} {error}",
                        t!("settings-save-failed-description")
                    ))
                    .autohide(false)
                    .action(|_, _, _| {
                        Button::new("retry-settings-save")
                            .label(t!("shell-updates-retry"))
                            .on_click(|_, window, cx| {
                                save_settings(window, cx).detach();
                            })
                    }),
                cx,
            );

            false
        }
    }
}

pub fn settings_view(editing: Entity<SettingsEditing>, cx: &App) -> Settings {
    let profiles = cx.global::<AppSettings>().config().profiles.list.clone();

    let agent_profiles = cx
        .global::<AppSettings>()
        .config()
        .agent_profiles
        .list
        .clone();

    let backdrop = cx
        .global::<AppSettings>()
        .config()
        .appearance
        .window_backdrop;

    let background_image_enabled = cx
        .global::<AppSettings>()
        .config()
        .appearance
        .background_image
        .is_some();

    #[cfg(windows)]
    let shell_integration_mismatched = shell_integration_dll_mismatched();

    #[cfg(not(windows))]
    let shell_integration_mismatched = false;

    let sidebar_style = sidebar_surface(cx).rounded_none().border_0().border_r_1();

    Settings::new("app-settings")
        .sidebar_width(SETTINGS_NAV_WIDTH)
        .sidebar_size_range(SETTINGS_NAV_WIDTH..SETTINGS_NAV_WIDTH)
        .sidebar_style(&sidebar_style)
        // Each subcategory is its own page; the alternative scrolls the
        // whole category top to bottom.
        .single_group_pages(true)
        .page(appearance_page(
            editing,
            backdrop,
            background_image_enabled,
            cx.global::<AppSettings>().config().appearance.tab_auto_size,
            cx.global::<AppSettings>()
                .config()
                .appearance
                .show_git_status_on_title_bar,
        ))
        .page(system_page(shell_integration_mismatched))
        .page(profiles_page(&profiles, &agent_profiles))
        .page(terminal_page())
        .page(agent_page(&agent_profiles, cx))
        .page(about_page())
}
