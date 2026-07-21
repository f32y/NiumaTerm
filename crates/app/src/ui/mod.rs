//! The GPUI root shell: the window-level entity owning the workspace/tab
//! model and all chrome (titlebar, sidebar, tab bar). Split by concern:
//! `persistence` (session save/restore), `sidebar` and `tab_bar` (chrome renderers).

pub(crate) use self::assets::AppAssets;
pub(crate) use self::settings::{
    AppSettings, apply_ui_theme, apply_window_translucency, background_image_layer_opacity,
    surface_background_opacity, watch_themes, window_background_appearance,
};
pub(crate) use self::shell::{
    CloseTab, NewTab, NewWindow, NewWorkspace, NextTab, NextWorkspace, PrevTab, PrevWorkspace,
    ResizePaneDown, ResizePaneLeft, ResizePaneRight, ResizePaneUp, Shell, ShowSettings, SplitDown,
    SplitLeft, SplitRight, SplitUp, ToggleSidebar,
};

mod assets;
mod auto_refresh;
mod codex_usage;
mod font_picker;
mod git_sidebar;
mod git_status;
mod persistence;
mod settings;
mod shell;
mod tab_bar;
mod token_usage;
mod workspace_sidebar;
