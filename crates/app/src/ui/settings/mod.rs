//! Persisted to `config.toml`: seeded via [`AppSettings::load`] at startup,
//! written back patch-style via [`AppSettings::save`] once when the settings
//! dialog closes (see `Shell::on_show_settings`). Field edits mutate the global
//! live for preview; only closing the dialog persists them.

mod about_page;
mod agent_page;
mod agent_profile_dialog;
mod agent_profile_list;
mod appearance_page;
mod card;
mod fields;
mod opacity;
mod profiles_page;
mod remote_session_page;
mod state;
mod system_page;
mod table;
mod terminal_page;
mod theme;

use std::{io, path};

#[cfg(test)]
use gpui::AppContext as _;
#[cfg(test)]
use gpui::WindowBackgroundAppearance;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, ClipboardItem, Div, FileDialogFilter, Global, InteractiveElement as _,
    IntoElement as _, ParentElement as _, PathPromptOptions, SharedString,
    StatefulInteractiveElement as _, StyleRefinement, Styled as _, Window, div, px, relative,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::dialog::{DialogClose, DialogFooter};
use gpui_component::group_box::{GroupBox, GroupBoxVariants as _};
use gpui_component::input::{Input, InputEvent};
use gpui_component::label::Label;
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::setting::{
    NumberFieldOptions, SettingField, SettingGroup, SettingItem, SettingPage, Settings,
};
use gpui_component::switch::Switch;
use gpui_component::{
    ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use nmt_agent_utils::HookInstallStatus;
use nmt_agent_utils::claude_code::hook as claude_hook;
use nmt_agent_utils::codex::hook as codex_hook;
use nmt_agent_utils::update::{DiscoverySupport, InstallationKey, ProviderKind, UpdatePhase};
#[cfg(test)]
use nmt_config::CursorShape;
use nmt_config::appearance::SmoothScrollingMode;
use nmt_config::remote_session::RemoteSessionConfig;
use nmt_config::system::{NewlineShortcut, WarnBeforeTerminatingShell};
use nmt_platform::{
    is_shell_integration_registered, register_shell_integration, set_system_notification_enabled,
    shell_integration_dll_mismatched, system_notification_enabled, unregister_shell_integration,
};
use tracing::warn;

use crate::agent_pane::updates as agent_updates;
use crate::ui::UI_RADIUS;
use crate::ui::settings::about_page::about_page;
use crate::ui::settings::agent_page::agent_page;
#[cfg(test)]
use crate::ui::settings::agent_page::{installation_update_title, installation_version_text};
use crate::ui::settings::agent_profile_dialog::open_agent_profile_dialog;
use crate::ui::settings::agent_profile_list::agent_profile_list;
use crate::ui::settings::appearance_page::appearance_page;
use crate::ui::settings::card::{card_row, card_text_input};
use crate::ui::settings::fields::{
    background_image_field, background_image_opacity_field, background_opacity_field,
};
#[allow(unused_imports)]
pub(crate) use crate::ui::settings::opacity::{
    background_image_layer_opacity, main_view_background_opacity, surface_background_opacity,
    window_background_appearance,
};
#[cfg(test)]
use crate::ui::settings::opacity::{
    effective_background_image_layer_opacity, effective_background_opacity,
    effective_main_view_background_opacity, effective_surface_background_opacity,
    window_background_appearance_for,
};
use crate::ui::settings::profiles_page::profiles_page;
pub(crate) use crate::ui::settings::remote_session_page::reconcile_remote_host;
use crate::ui::settings::remote_session_page::remote_session_page;
pub(crate) use crate::ui::settings::state::builtin_agent_profile;
pub use crate::ui::settings::state::{
    AgentProfile, AgentProfileKind, AppSettings, EnvVar, InputStyle, Language, Profile,
    WindowBackdrop,
};
#[cfg(test)]
use crate::ui::settings::state::{
    DEFAULT_BACKGROUND_IMAGE_OPACITY, clamp_background_image_opacity, clamp_background_opacity,
    clamp_terminal_font_size, clamp_terminal_line_height, terminal_font_or_default,
    ui_font_or_default,
};
#[allow(unused_imports)]
pub use crate::ui::settings::state::{
    DEFAULT_FONT_FAMILY, DEFAULT_FONT_SIZE, DEFAULT_LINE_HEIGHT, DEFAULT_SHELL, DEFAULT_TAB_WIDTH,
    DEFAULT_UI_FONT,
};
use crate::ui::settings::state::{
    agent_kind_display_label, clamp_git_interval, clamp_tab_width, cursor_shape_from_value,
    input_style_from_value, input_style_label,
};
use crate::ui::settings::system_page::system_page;
use crate::ui::settings::table::{
    ENV_OPERATION_COLUMN, TABLE_OPERATION_BUTTON, TrashIcon, table_frame, table_header, table_row,
};
use crate::ui::settings::terminal_page::terminal_page;
#[cfg(test)]
use crate::ui::settings::theme::tab_background_opacity;
use crate::ui::settings::theme::theme_list;
pub(crate) use crate::ui::settings::theme::{
    apply_ui_theme, apply_window_translucency, watch_themes,
};
use crate::{PlatformHandle, ui};

const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const RELEASE_PAGE_URL: &str = "https://github.com/f32y/NiumaTerm/releases";

pub const MAX_TAB_WIDTH: f64 = DEFAULT_TAB_WIDTH * 3.0;

pub fn settings_view(cx: &App) -> Settings {
    let profiles = cx.global::<AppSettings>().profiles.clone();
    let agent_profiles = cx.global::<AppSettings>().agent_profiles.clone();
    let backdrop = cx.global::<AppSettings>().window_backdrop;
    let background_image_enabled = cx.global::<AppSettings>().background_image.is_some();
    let shell_integration_mismatched = shell_integration_dll_mismatched();

    let sidebar_style = StyleRefinement::default()
        .bg(cx.theme().sidebar)
        .border_t_1()
        .border_b_1()
        .border_l_1()
        .border_color(cx.theme().sidebar_border)
        .rounded(UI_RADIUS)
        .overflow_hidden();

    Settings::new("app-settings")
        .sidebar_width(px(240.0))
        .sidebar_style(&sidebar_style)
        // Each subcategory is its own page; the alternative scrolls the
        // whole category top to bottom.
        .single_group_pages(true)
        .page(terminal_page())
        .page(appearance_page(
            backdrop,
            background_image_enabled,
            cx.global::<AppSettings>().tab_auto_size,
            cx.global::<AppSettings>().show_git_status_on_title_bar,
        ))
        .page(profiles_page(&profiles, &agent_profiles))
        .page(agent_page(&agent_profiles, cx))
        .page(system_page(shell_integration_mismatched))
        .page(remote_session_page())
        .page(about_page())
}

#[cfg(test)]
mod tests;
