use std::sync::LazyLock;

use gpui::{Font, FontFallbacks, Pixels, SharedString, font, px};

pub(crate) const UI_RADIUS: Pixels = px(8.0);
pub(crate) const UI_BORDER_OPACITY: f32 = 0.5;
const DEFAULT_CJK_FONT_FAMILY: &str = "Microsoft YaHei";

static DEFAULT_FONT_FALLBACKS: LazyLock<FontFallbacks> =
    LazyLock::new(|| FontFallbacks::from_fonts(vec![DEFAULT_CJK_FONT_FAMILY.to_string()]));

/// Prefer one Chinese font across application surfaces before DirectWrite
/// continues through its system list for characters that remain unsupported.
pub(crate) fn font_with_default_fallback(family: impl Into<SharedString>) -> Font {
    let mut font = font(family);

    font.fallbacks = Some(DEFAULT_FONT_FALLBACKS.clone());
    font
}

pub(crate) fn default_font_fallbacks() -> FontFallbacks {
    DEFAULT_FONT_FALLBACKS.clone()
}

pub(crate) use crate::ui::active_list::{ActiveList, HasId};
pub(crate) use crate::ui::assets::AppAssets;
pub(crate) use crate::ui::git_status::current_branch;
pub(crate) use crate::ui::settings::{
    AppSettings, CollapseRows, apply_ui_theme, apply_window_translucency,
    background_image_layer_opacity, main_view_background_opacity, watch_themes,
    window_background_appearance,
};
pub(crate) use crate::ui::shell::{
    CloseTab, NewAgentTab, NewRemoteTab, NewTab, NewWindow, NewWorkspace, NextTab, NextWorkspace,
    PrevTab, PrevWorkspace, ResizePaneDown, ResizePaneLeft, ResizePaneRight, ResizePaneUp, Shell,
    ShowSettings, SplitDown, SplitLeft, SplitRight, SplitUp, TabSurface, ToggleSidebar,
};
pub(crate) use crate::ui::working_indicator::WorkingIndicator;

mod active_list;
mod assets;
mod auto_refresh;
mod background_tasks;
mod floating_surface;
mod font_picker;
mod git_sidebar;
mod git_status;
mod persistence;
mod right_panel;
mod settings;
mod shell;
mod sidebar_resize;
mod tab_bar;
mod terminal_status;
#[cfg(test)]
mod tests;
mod token_usage;
mod workflows;
mod working_indicator;
mod workspace_sidebar;
