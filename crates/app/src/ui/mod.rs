use std::sync::LazyLock;

use gpui::{App, Font, FontFallbacks, Pixels, SharedString, font, px};
pub(crate) use gpui_component::modern_menu::dismiss_modern_menu;
use gpui_component::modern_menu::{prewarm_modern_menu, set_default_font};

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

/// Keep the context menu window built and carrying the chrome font.
///
/// The menu is drawn in a window of its own, so it inherits no text style from
/// the window it opens over and would otherwise miss the CJK preference the rest
/// of the chrome carries. Both halves are cheap once they have taken effect, so
/// this is called from the shell's render and follows a font setting that
/// changes underneath it.
pub(crate) fn sync_modern_menu(cx: &mut App) {
    let font = font_with_default_fallback(cx.global::<AppSettings>().ui_font_family.clone());

    set_default_font(cx, font);
    prewarm_modern_menu(cx);
}

pub(crate) fn default_font_fallbacks() -> FontFallbacks {
    DEFAULT_FONT_FALLBACKS.clone()
}

pub(crate) use crate::ui::active_list::{ActiveList, HasId};
pub(crate) use crate::ui::assets::AppAssets;
pub(crate) use crate::ui::modern_dropdown::modern_dropdown;
pub(crate) use crate::ui::settings::{
    AppSettings, apply_ui_theme, apply_window_translucency, background_image_layer_opacity,
    install_agent_settings, install_terminal_settings, main_view_background_opacity, watch_themes,
    window_background_appearance,
};
pub(crate) use crate::ui::shell::{
    CloseTab, NewAgentTab, NewRemoteTab, NewTab, NewWindow, NewWorkspace, NextTab, NextWorkspace,
    PrevTab, PrevWorkspace, ResizePaneDown, ResizePaneLeft, ResizePaneRight, ResizePaneUp, Shell,
    ShowSettings, SplitDown, SplitLeft, SplitRight, SplitUp, TabSurface, ToggleSidebar,
};

mod active_list;
mod assets;
mod auto_refresh;
mod background_tasks;
mod composition;
mod floating_surface;
mod fluent;
mod font_picker;
mod git_sidebar;
mod git_status;
mod modern_dropdown;
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
mod workspace_sidebar;
