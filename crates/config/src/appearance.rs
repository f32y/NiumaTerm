//! Visual settings persisted as the `[appearance]` section of `config.toml`.

use serde::{Deserialize, Deserializer, Serialize};

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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum SmoothScrollingMode {
    #[default]
    All,
    OnlyTerminal,
    OnlyAgent,
    Off,
}

impl SmoothScrollingMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::OnlyTerminal => "only-terminal",
            Self::OnlyAgent => "only-agent",
            Self::Off => "off",
        }
    }

    pub fn from_value(value: &str) -> Self {
        match value {
            "only-terminal" => Self::OnlyTerminal,
            "only-agent" => Self::OnlyAgent,
            "off" => Self::Off,
            _ => Self::All,
        }
    }

    pub fn terminal_enabled(self) -> bool {
        matches!(self, Self::All | Self::OnlyTerminal)
    }

    pub fn agent_enabled(self) -> bool {
        matches!(self, Self::All | Self::OnlyAgent)
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum SmoothScrollingValue {
    Mode(SmoothScrollingMode),
    Legacy(bool),
}

fn deserialize_smooth_scrolling<'de, D>(deserializer: D) -> Result<SmoothScrollingMode, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(match SmoothScrollingValue::deserialize(deserializer)? {
        SmoothScrollingValue::Mode(mode) => mode,
        SmoothScrollingValue::Legacy(true) => SmoothScrollingMode::All,
        SmoothScrollingValue::Legacy(false) => SmoothScrollingMode::Off,
    })
}

/// Where the tab strip lives. Vertical folds the tabs into the workspace
/// sidebar as child rows of the workspace that owns them, which frees the
/// title bar row entirely.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum TabBarStyle {
    /// A row of tabs across the title bar.
    #[default]
    Horizontal,
    /// Tabs nested under their workspace in the sidebar.
    Vertical,
}

impl TabBarStyle {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Horizontal => "horizontal",
            Self::Vertical => "vertical",
        }
    }

    pub fn from_value(value: &str) -> Self {
        match value {
            "vertical" => Self::Vertical,
            _ => Self::Horizontal,
        }
    }
}

/// The window backdrop material. The opacity slider applies in every mode;
/// the mode only selects what shows through translucent content.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum WindowBackdrop {
    /// Windows 11 Mica material: a static tint, no blur of the content behind.
    Mica,
    /// Blur the content behind the window (Acrylic).
    #[default]
    Acrylic,
    /// No material; translucent content shows the desktop directly.
    Off,
}

impl WindowBackdrop {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mica => "mica",
            Self::Acrylic => "acrylic",
            Self::Off => "off",
        }
    }

    pub fn from_value(value: &str) -> Self {
        match value {
            "mica" => Self::Mica,
            "off" => Self::Off,
            _ => Self::Acrylic,
        }
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum WindowBackdropValue {
    Mode(WindowBackdrop),
    Legacy(bool),
}

fn deserialize_window_backdrop<'de, D>(deserializer: D) -> Result<WindowBackdrop, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(match WindowBackdropValue::deserialize(deserializer)? {
        WindowBackdropValue::Mode(mode) => mode,
        // Legacy `enable-window-transparency` boolean: on kept the acrylic
        // + opacity behavior, off was fully opaque.
        WindowBackdropValue::Legacy(true) => WindowBackdrop::Acrylic,
        WindowBackdropValue::Legacy(false) => WindowBackdrop::Off,
    })
}

/// UI display language, stored as its BCP 47 tag in `config.toml`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum Language {
    #[default]
    #[serde(rename = "en")]
    En,
    #[serde(rename = "zh-CN")]
    ZhCn,
}

impl Language {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::ZhCn => "zh-CN",
        }
    }

    pub fn from_value(value: &str) -> Self {
        match value {
            "zh-CN" => Self::ZhCn,
            _ => Self::En,
        }
    }
}

fn deserialize_language<'de, D>(deserializer: D) -> Result<Language, D::Error>
where
    D: Deserializer<'de>,
{
    // A hand-edited or future language tag must load as English instead of
    // failing the whole config parse.
    Ok(Language::from_value(&String::deserialize(deserializer)?))
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

fn default_scroll_to_bottom_when_typing() -> bool {
    true
}

fn default_agent_font_family() -> String {
    default_terminal_font_family()
}

fn default_agent_font_size() -> f64 {
    default_terminal_font_size()
}

fn default_background_opacity() -> f64 {
    1.0
}

fn default_background_image_opacity() -> f64 {
    0.3
}

fn default_window_backdrop() -> WindowBackdrop {
    WindowBackdrop::Acrylic
}

fn default_transparent_main_view() -> bool {
    true
}

/// The `[appearance]` section: visual settings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AppearanceConfig {
    #[serde(default, rename = "input-style")]
    pub input_style: InputStyle,
    /// Move a scrolled viewport to the latest output after typed input.
    #[serde(
        default = "default_scroll_to_bottom_when_typing",
        rename = "scroll-to-bottom-when-typing"
    )]
    pub scroll_to_bottom_when_typing: bool,
    /// Use the terminal theme background for Agent Pane.
    #[serde(default, rename = "agent-pane-use-terminal-background")]
    pub agent_pane_use_terminal_background: bool,
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
    /// Shrink tabs toward a minimum as the strip fills, instead of holding
    /// `tab_width`.
    #[serde(default, rename = "tab-auto-size")]
    pub tab_auto_size: bool,
    /// Tab strip placement: a horizontal row in the title bar, or vertical
    /// rows nested under each workspace in the sidebar.
    #[serde(default, rename = "tab-bar-style")]
    pub tab_bar_style: TabBarStyle,
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
    /// Font family used by agent (chat) tabs.
    #[serde(default = "default_agent_font_family", rename = "agent-font-family")]
    pub agent_font_family: String,
    /// Font size in pixels used by agent (chat) tabs.
    #[serde(default = "default_agent_font_size", rename = "agent-font-size")]
    pub agent_font_size: f64,
    /// Whether terminal font pickers only show monospace fonts.
    #[serde(default = "default_monospace_only", rename = "monospace-only")]
    pub monospace_only: bool,
    /// Window backdrop material. Acrylic is the default so existing opacity
    /// configurations keep working; legacy booleans deserialize as `true` →
    /// Acrylic and `false` → Off.
    #[serde(
        default = "default_window_backdrop",
        rename = "enable-window-transparency",
        deserialize_with = "deserialize_window_backdrop"
    )]
    pub window_backdrop: WindowBackdrop,
    /// Allow the Terminal View and Agent Pane background to show content behind it.
    #[serde(
        default = "default_transparent_main_view",
        rename = "transparent-main-view"
    )]
    pub transparent_main_view: bool,
    /// Select which scrolling views animate line-based mouse-wheel input.
    #[serde(
        default,
        rename = "smooth-scrolling",
        deserialize_with = "deserialize_smooth_scrolling"
    )]
    pub smooth_scrolling: SmoothScrollingMode,
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
    /// UI display language.
    #[serde(
        default,
        rename = "language",
        deserialize_with = "deserialize_language"
    )]
    pub language: Language,
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
            scroll_to_bottom_when_typing: true,
            agent_pane_use_terminal_background: false,
            command_blocks: true,
            show_daily_token_usage: false,
            show_git_status_on_title_bar: false,
            git_status_refresh_interval: default_git_status_refresh_interval(),
            tab_width: default_tab_width(),
            tab_auto_size: false,
            tab_bar_style: TabBarStyle::default(),
            ui_font: default_ui_font(),
            terminal_font_family: default_terminal_font_family(),
            terminal_font_size: default_terminal_font_size(),
            terminal_line_height: default_terminal_line_height(),
            agent_font_family: default_agent_font_family(),
            agent_font_size: default_agent_font_size(),
            monospace_only: true,
            window_backdrop: default_window_backdrop(),
            transparent_main_view: default_transparent_main_view(),
            smooth_scrolling: SmoothScrollingMode::default(),
            background_opacity: default_background_opacity(),
            background_image: None,
            background_image_opacity: default_background_image_opacity(),
            language: Language::default(),
        }
    }
}
