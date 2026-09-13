//! Persisted to `config.toml`: seeded via [`AppSettings::load`] at startup,
//! written back patch-style via [`AppSettings::save`] when the settings
//! workspace is left or closed. Field edits mutate the global live for preview;
//! failed writes retain those edits and expose a retry action.

pub use crate::ui::settings::state::{
    AgentProfile, AgentProfileKind, AgentProfileLauncher, AppSettings, CollapseRows, EnvVar,
    InputStyle, MIN_TAB_WIDTH, ModelListStyle, Profile, TabBarStyle, WindowBackdrop,
};

pub(crate) use crate::ui::settings::opacity::{
    background_image_layer_opacity, main_view_background_opacity, window_background_appearance,
};
#[cfg(windows)]
pub(crate) use crate::ui::settings::remote_session_page::reconcile_remote_host;
pub(crate) use crate::ui::settings::state::builtin_agent_profile;
#[cfg(test)]
pub(crate) use crate::ui::settings::state::{
    DEFAULT_FONT_FAMILY, DEFAULT_FONT_SIZE, DEFAULT_LINE_HEIGHT, DEFAULT_UI_FONT,
};
pub(crate) use crate::ui::settings::terminal_bridge::{
    install_agent_settings, install_terminal_settings,
};
pub(crate) use crate::ui::settings::theme::{
    apply_ui_theme, apply_window_translucency, watch_themes,
};

mod about_page;
mod agent_page;
mod agent_profile_dialog;
mod agent_profile_list;
mod appearance_page;
mod card;
mod fields;
mod opacity;
mod profiles_page;
#[cfg(windows)]
mod remote_session_page;
mod state;
mod system_page;
mod table;
mod terminal_bridge;
mod terminal_page;
mod theme;

#[cfg(test)]
mod tests;

use std::{io, path};

#[cfg(test)]
use gpui::AppContext as _;
#[cfg(test)]
use gpui::WindowBackgroundAppearance;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, Div, FileDialogFilter, InteractiveElement as _, IntoElement as _,
    ParentElement as _, PathPromptOptions, SharedString, StatefulInteractiveElement as _,
    Styled as _, Window, div, px, relative,
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
    NumberFieldOptions, SettingField, SettingGroup, SettingItem, SettingPage, Settings,
};
use gpui_component::switch::Switch;
use gpui_component::{
    ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use nmt_agent::HookInstallStatus;
use nmt_agent::claude_code::hook as claude_hook;
use nmt_agent::codex::hook as codex_hook;
use nmt_agent::update::{DiscoverySupport, InstallationKey, ProviderKind, UpdatePhase};
#[cfg(test)]
use nmt_config::CursorShape;
use nmt_config::system::{NewlineShortcut, WarnBeforeTerminatingShell};
use nmt_platform::{
    is_shell_integration_registered, register_shell_integration, set_system_notification_enabled,
    shell_integration_dll_mismatched, system_notification_enabled, unregister_shell_integration,
};
use rust_i18n::t;
use tracing::warn;

#[cfg(windows)]
#[cfg(windows)]
use crate::PlatformHandle;
#[cfg(windows)]
use crate::ui::UI_RADIUS;
use crate::ui::composition::sidebar_surface;
use crate::ui::settings::about_page::about_page;
use crate::ui::settings::agent_page::agent_page;
#[cfg(test)]
use crate::ui::settings::agent_page::{installation_update_title, installation_version_text};
use crate::ui::settings::agent_profile_dialog::open_agent_profile_dialog;
use crate::ui::settings::agent_profile_list::agent_profile_list;
use crate::ui::settings::appearance_page::appearance_page;
use crate::ui::settings::card::{card_row, card_text_input, description_hint};
use crate::ui::settings::fields::{
    background_image_field, background_image_opacity_field, background_opacity_field,
};
#[cfg(test)]
use crate::ui::settings::opacity::{
    effective_background_image_layer_opacity, effective_background_opacity,
    effective_main_view_background_opacity, effective_surface_background_opacity,
    window_background_appearance_for,
};
use crate::ui::settings::profiles_page::profiles_page;
#[cfg(windows)]
use crate::ui::settings::remote_session_page::remote_session_page;
#[cfg(test)]
use crate::ui::settings::state::{
    DEFAULT_AGENT_TRANSCRIPT_FONT_SIZE, DEFAULT_BACKGROUND_IMAGE_OPACITY, DEFAULT_TAB_WIDTH,
    clamp_agent_transcript_font_size, clamp_background_image_opacity, clamp_background_opacity,
    clamp_terminal_font_size, clamp_terminal_line_height, terminal_font_or_default,
    ui_font_or_default,
};
use crate::ui::settings::state::{
    agent_kind_display_label, clamp_git_interval, clamp_tab_width, input_style_label,
};
use crate::ui::settings::system_page::system_page;
use crate::ui::settings::table::{
    ENV_OPERATION_COLUMN, TABLE_OPERATION_BUTTON, TrashIcon, table_frame, table_header, table_row,
};
use crate::ui::settings::terminal_page::terminal_page;
#[cfg(test)]
use crate::ui::settings::theme::tab_background_opacity;
use crate::ui::settings::theme::theme_list;
use crate::{agent_updates, ui};

const APP_VERSION: &str = env!("NIUMATERM_VERSION");
const APP_INTERNAL_VERSION: &str = env!("NIUMATERM_INTERNAL_VERSION");
const RELEASE_PAGE_URL: &str = "https://github.com/f32y/NiumaTerm/releases";

pub const MAX_TAB_WIDTH: f64 = MIN_TAB_WIDTH * 3.0;

struct SettingsSaveFailure;

/// Keep failed edits in memory and offer another write after the user fixes
/// the configuration file or its permissions.
pub(crate) fn save_settings(window: &mut Window, cx: &mut App) -> bool {
    match cx.global_mut::<AppSettings>().save() {
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
                                save_settings(window, cx);
                            })
                    }),
                cx,
            );

            false
        }
    }
}

pub fn settings_view(cx: &App) -> Settings {
    let profiles = cx.global::<AppSettings>().profiles.clone();
    let agent_profiles = cx.global::<AppSettings>().agent_profiles.clone();
    let backdrop = cx.global::<AppSettings>().appearance.window_backdrop;

    let background_image_enabled = cx
        .global::<AppSettings>()
        .appearance
        .background_image
        .is_some();

    let shell_integration_mismatched = shell_integration_dll_mismatched();

    let sidebar_style = sidebar_surface(cx).border_r_0();

    let settings = Settings::new("app-settings")
        .sidebar_width(px(240.0))
        .sidebar_style(&sidebar_style)
        // Each subcategory is its own page; the alternative scrolls the
        // whole category top to bottom.
        .single_group_pages(true)
        .page(appearance_page(
            backdrop,
            background_image_enabled,
            cx.global::<AppSettings>().appearance.tab_auto_size,
            cx.global::<AppSettings>()
                .appearance
                .show_git_status_on_title_bar,
        ))
        .page(system_page(shell_integration_mismatched))
        .page(profiles_page(&profiles, &agent_profiles))
        .page(terminal_page())
        .page(agent_page(&agent_profiles, cx));

    // Remote sessions are hosted by ConPTY and keyed by DPAPI, so the page
    // that configures them exists only where they do.
    #[cfg(windows)]
    let settings = settings.page(remote_session_page());

    settings.page(about_page())
}
