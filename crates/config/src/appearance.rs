//! Visual settings persisted as the `[appearance]` section of `config.toml`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum InputStyle {
    #[default]
    Waterfall,
    FixedBottom,
}

impl InputStyle {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Waterfall => "waterfall",
            Self::FixedBottom => "fixed-bottom",
        }
    }

    pub fn is_fixed_bottom(self) -> bool {
        matches!(self, Self::FixedBottom)
    }
}

fn default_git_status_refresh_interval() -> u64 {
    30
}

fn default_tab_width() -> f64 {
    120.0
}

fn default_ui_font() -> String {
    "Segoe UI".to_string()
}

fn default_terminal_font_family() -> String {
    "Consolas".to_string()
}

fn default_terminal_font_size() -> f64 {
    14.0
}

fn default_terminal_line_height() -> f64 {
    1.0
}

fn default_background_opacity() -> f64 {
    1.0
}

fn default_background_image_opacity() -> f64 {
    0.3
}

fn default_window_transparency_enabled() -> bool {
    true
}

/// The `[appearance]` section: visual settings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AppearanceConfig {
    #[serde(default, rename = "input-style")]
    pub input_style: InputStyle,
    /// Render command blocks in the grid (separators, exit status, gutter;
    /// command-blocks-rendering).
    #[serde(default = "default_command_blocks", rename = "command-blocks")]
    pub command_blocks: bool,
    /// Show today's ccusage token totals in the titlebar.
    #[serde(default, rename = "show-daily-token-usage")]
    pub show_daily_token_usage: bool,
    /// Show the git `+added -removed` line counts in the titlebar.
    #[serde(default, rename = "show-git-status-on-title-bar")]
    pub show_git_status_on_title_bar: bool,
    /// Seconds between git status refreshes (10/15/30/60; clamped on load).
    #[serde(
        default = "default_git_status_refresh_interval",
        rename = "git-status-refresh-interval"
    )]
    pub git_status_refresh_interval: u64,
    /// Fixed tab width in pixels (120–360; clamped on load).
    #[serde(default = "default_tab_width", rename = "tab-width")]
    pub tab_width: f64,
    /// Font family for the app chrome (titlebar, sidebar, tabs, dialogs).
    #[serde(default = "default_ui_font", rename = "ui-font")]
    pub ui_font: String,
    /// Font family used by terminal panes.
    #[serde(
        default = "default_terminal_font_family",
        rename = "terminal-font-family"
    )]
    pub terminal_font_family: String,
    /// Font size in pixels used by terminal panes.
    #[serde(default = "default_terminal_font_size", rename = "terminal-font-size")]
    pub terminal_font_size: f64,
    /// Terminal line height as a multiplier on font size.
    #[serde(
        default = "default_terminal_line_height",
        rename = "terminal-line-height"
    )]
    pub terminal_line_height: f64,
    /// Whether terminal font pickers only show monospace fonts.
    #[serde(default = "default_monospace_only", rename = "monospace-only")]
    pub monospace_only: bool,
    /// Allow the window to use an alpha-capable render target and acrylic
    /// backdrop. Defaults on so existing opacity configurations keep working.
    #[serde(
        default = "default_window_transparency_enabled",
        rename = "enable-window-transparency"
    )]
    pub window_transparency_enabled: bool,
    /// Whole-window background opacity (0.2–1.0; clamped on load).
    #[serde(default = "default_background_opacity", rename = "background-opacity")]
    pub background_opacity: f64,
    /// Local image drawn behind all window content.
    #[serde(
        default,
        rename = "background-image",
        skip_serializing_if = "Option::is_none"
    )]
    pub background_image: Option<String>,
    /// How strongly the image shows through the window surfaces (0.0–1.0).
    #[serde(
        default = "default_background_image_opacity",
        rename = "background-image-opacity"
    )]
    pub background_image_opacity: f64,
}

fn default_command_blocks() -> bool {
    true
}

fn default_monospace_only() -> bool {
    true
}

impl Default for AppearanceConfig {
    fn default() -> Self {
        Self {
            input_style: InputStyle::default(),
            command_blocks: true,
            show_daily_token_usage: false,
            show_git_status_on_title_bar: false,
            git_status_refresh_interval: default_git_status_refresh_interval(),
            tab_width: default_tab_width(),
            ui_font: default_ui_font(),
            terminal_font_family: default_terminal_font_family(),
            terminal_font_size: default_terminal_font_size(),
            terminal_line_height: default_terminal_line_height(),
            monospace_only: true,
            window_transparency_enabled: default_window_transparency_enabled(),
            background_opacity: default_background_opacity(),
            background_image: None,
            background_image_opacity: default_background_image_opacity(),
        }
    }
}
