//! Visual settings persisted as the `[appearance]` section of `config.toml`.

use serde::{Deserialize, Deserializer, Serialize};

use crate::defaults::default_bool_true;

/// Default font families and metrics shared by loading and editing.
#[cfg(target_os = "windows")]
pub const DEFAULT_FONT_FAMILY: &str = "Consolas";

#[cfg(target_os = "macos")]
pub const DEFAULT_FONT_FAMILY: &str = "Menlo";

#[cfg(all(unix, not(target_os = "macos")))]
pub const DEFAULT_FONT_FAMILY: &str = "monospace";

pub const DEFAULT_FONT_SIZE: f64 = 14.0;
pub const DEFAULT_AGENT_TRANSCRIPT_FONT_SIZE: f64 = 13.0;
pub const DEFAULT_LINE_HEIGHT: f64 = 1.0;
pub const DEFAULT_BACKGROUND_IMAGE_OPACITY: f64 = 0.3;

#[cfg(target_os = "windows")]
pub const DEFAULT_UI_FONT: &str = "Segoe UI";

#[cfg(not(target_os = "windows"))]
pub const DEFAULT_UI_FONT: &str = ".SystemUIFont";

pub const MIN_TAB_WIDTH: f64 = 120.0;
pub const DEFAULT_TAB_WIDTH: f64 = 220.0;

pub const MAX_TAB_WIDTH: f64 = MIN_TAB_WIDTH * 3.0;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum InputStyle {
    #[default]
    Waterfall,
    FixedBottom,
}

impl InputStyle {
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
    pub fn terminal_enabled(self) -> bool {
        matches!(self, Self::All | Self::OnlyTerminal)
    }

    pub fn agent_enabled(self) -> bool {
        matches!(self, Self::All | Self::OnlyAgent)
    }

    /// Every other scrollable surface: sidebars, settings pages, pickers and
    /// the lists inside panels. Only `All` covers them, because each of the
    /// two narrow modes names one surface and means that one.
    pub fn panels_enabled(self) -> bool {
        matches!(self, Self::All)
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

/// The window backdrop material. Acrylic honors the opacity slider; the Mica
/// variants hand the chrome entirely to DWM, and Off keeps the window opaque.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum WindowBackdrop {
    /// Windows 11 Mica Alt material: the same wallpaper tint as Mica, drawn
    /// stronger for tabbed shells.
    MicaAlt,

    /// Windows 11 Mica material: a static tint, no blur of the content behind.
    Mica,

    /// Blur the content behind the window (Acrylic).
    #[default]
    Acrylic,

    /// No material; translucent content shows the desktop directly.
    Off,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum WindowBackdropValue {
    // Parsed as a plain string rather than as the enum so that an unknown
    // material degrades to a usable window instead of failing the whole file.
    Mode(String),
    Legacy(bool),
}

fn deserialize_window_backdrop<'de, D>(deserializer: D) -> Result<WindowBackdrop, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(match WindowBackdropValue::deserialize(deserializer)? {
        WindowBackdropValue::Mode(mode) => mode.as_str().into(),
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

fn deserialize_language<'de, D>(deserializer: D) -> Result<Language, D::Error>
where
    D: Deserializer<'de>,
{
    // A hand-edited or future language tag must load as English instead of
    // failing the whole config parse.
    Ok(String::deserialize(deserializer)?.as_str().into())
}

fn default_git_status_refresh_interval() -> u64 {
    30
}

fn default_tab_width() -> f64 {
    DEFAULT_TAB_WIDTH
}

/// The proportional face the interface is drawn in.
///
/// Named through GPUI's `.SystemUIFont` token rather than by family wherever
/// the system face is not a family the font list carries: macOS keeps San
/// Francisco out of the enumerable families, and asking for a name that is not
/// installed does not fail — it silently resolves to Helvetica.
#[cfg(target_os = "windows")]
fn default_ui_font() -> String {
    DEFAULT_UI_FONT.to_string()
}

#[cfg(not(target_os = "windows"))]
fn default_ui_font() -> String {
    DEFAULT_UI_FONT.to_string()
}

/// The fixed-pitch face the terminal grid is drawn in.
///
/// `Menlo` is the monospace face macOS installs and exposes by name. `SF Mono`
/// ships with the system but, like the UI face, is not resolvable as a family,
/// and a terminal that silently fell back to Helvetica would draw its grid in
/// a proportional face.
#[cfg(target_os = "windows")]
fn default_terminal_font_family() -> String {
    DEFAULT_FONT_FAMILY.to_string()
}

#[cfg(target_os = "macos")]
fn default_terminal_font_family() -> String {
    DEFAULT_FONT_FAMILY.to_string()
}

#[cfg(all(unix, not(target_os = "macos")))]
fn default_terminal_font_family() -> String {
    DEFAULT_FONT_FAMILY.to_string()
}

fn default_terminal_font_size() -> f64 {
    DEFAULT_FONT_SIZE
}

fn default_terminal_line_height() -> f64 {
    DEFAULT_LINE_HEIGHT
}

fn default_scroll_to_bottom_when_typing() -> bool {
    true
}

/// Agent transcript prose is chat text, not terminal output: it wraps at a
/// reading measure and mixes Latin with CJK, both of which a proportional UI
/// face sets better than the fixed-pitch terminal face. Code inside a
/// transcript keeps its own family through `agent-transcript-font-family`.
fn default_agent_font_family() -> String {
    default_ui_font()
}

fn default_agent_font_size() -> f64 {
    default_terminal_font_size()
}

fn default_agent_transcript_font_family() -> String {
    default_terminal_font_family()
}

fn default_agent_transcript_font_size() -> f64 {
    DEFAULT_AGENT_TRANSCRIPT_FONT_SIZE
}

fn default_background_opacity() -> f64 {
    1.0
}

fn default_background_image_opacity() -> f64 {
    DEFAULT_BACKGROUND_IMAGE_OPACITY
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

    /// Whether terminal and agent transcript font pickers only show monospace fonts.
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

    /// Font family used by code-oriented agent transcript content.
    #[serde(
        default = "default_agent_transcript_font_family",
        rename = "agent-transcript-font-family"
    )]
    pub agent_transcript_font_family: String,

    /// Font size in pixels used by code-oriented agent transcript content.
    #[serde(
        default = "default_agent_transcript_font_size",
        rename = "agent-transcript-font-size"
    )]
    pub agent_transcript_font_size: f64,

    /// Put disclosed content on screen at once, skipping the entrance the
    /// transcript otherwise plays for it.
    #[serde(default, rename = "reduce-motion")]
    pub reduce_motion: bool,

    /// Hold the agent conversation column at a reading width and centre it.
    /// Off, the column follows the pane width with a fixed margin each side.
    #[serde(
        default = "default_bool_true",
        rename = "human-friendly-agent-ui-layout"
    )]
    pub human_friendly_agent_ui_layout: bool,
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
            agent_transcript_font_family: default_agent_transcript_font_family(),
            agent_transcript_font_size: default_agent_transcript_font_size(),
            reduce_motion: false,
            human_friendly_agent_ui_layout: true,
        }
    }
}

impl From<InputStyle> for &'static str {
    fn from(value: InputStyle) -> Self {
        match value {
            InputStyle::Waterfall => "waterfall",
            InputStyle::FixedBottom => "fixed-bottom",
        }
    }
}

impl From<SmoothScrollingMode> for &'static str {
    fn from(value: SmoothScrollingMode) -> Self {
        match value {
            SmoothScrollingMode::All => "all",
            SmoothScrollingMode::OnlyTerminal => "only-terminal",
            SmoothScrollingMode::OnlyAgent => "only-agent",
            SmoothScrollingMode::Off => "off",
        }
    }
}

impl From<&str> for SmoothScrollingMode {
    fn from(value: &str) -> Self {
        match value {
            "only-terminal" => Self::OnlyTerminal,
            "only-agent" => Self::OnlyAgent,
            "off" => Self::Off,
            _ => Self::All,
        }
    }
}

impl From<TabBarStyle> for &'static str {
    fn from(value: TabBarStyle) -> Self {
        match value {
            TabBarStyle::Horizontal => "horizontal",
            TabBarStyle::Vertical => "vertical",
        }
    }
}

impl From<&str> for TabBarStyle {
    fn from(value: &str) -> Self {
        match value {
            "vertical" => Self::Vertical,
            _ => Self::Horizontal,
        }
    }
}

impl From<WindowBackdrop> for &'static str {
    fn from(value: WindowBackdrop) -> Self {
        match value {
            WindowBackdrop::MicaAlt => "mica-alt",
            WindowBackdrop::Mica => "mica",
            WindowBackdrop::Acrylic => "acrylic",
            WindowBackdrop::Off => "off",
        }
    }
}

impl From<&str> for WindowBackdrop {
    /// An unrecognized name falls back to Off. It most likely comes from a
    /// newer build that knows a material this one does not, and an opaque
    /// window is guaranteed to render; guessing at a translucent mode is not.
    fn from(value: &str) -> Self {
        match value {
            "mica-alt" => Self::MicaAlt,
            "mica" => Self::Mica,
            "acrylic" => Self::Acrylic,
            _ => Self::Off,
        }
    }
}

impl From<Language> for &'static str {
    fn from(value: Language) -> Self {
        match value {
            Language::En => "en",
            Language::ZhCn => "zh-CN",
        }
    }
}

impl From<&str> for Language {
    fn from(value: &str) -> Self {
        match value {
            "zh-CN" => Self::ZhCn,
            _ => Self::En,
        }
    }
}

impl From<&str> for InputStyle {
    fn from(value: &str) -> Self {
        match value {
            "fixed-bottom" => InputStyle::FixedBottom,
            _ => InputStyle::Waterfall,
        }
    }
}

/// Snap a persisted refresh interval to the allowed set, falling back to 30.
pub fn clamp_git_interval(seconds: u64) -> u64 {
    if matches!(seconds, 10 | 15 | 30 | 60) {
        seconds
    } else {
        30
    }
}

/// The configured UI font, or the default when the config leaves it blank
/// (an empty family would fall back to gpui's default, not Segoe UI).
pub fn ui_font_or_default(family: &str) -> String {
    if family.trim().is_empty() {
        DEFAULT_UI_FONT.into()
    } else {
        family.to_string()
    }
}

pub fn terminal_font_or_default(family: &str) -> String {
    if family.trim().is_empty() {
        DEFAULT_FONT_FAMILY.into()
    } else {
        family.to_string()
    }
}

/// Clamp a persisted tab width to the allowed range, falling back to the
/// default for non-finite values.
pub fn clamp_tab_width(width: f64) -> f64 {
    if width.is_finite() {
        width.clamp(MIN_TAB_WIDTH, MAX_TAB_WIDTH)
    } else {
        DEFAULT_TAB_WIDTH
    }
}

pub fn clamp_terminal_font_size(size: f64) -> f64 {
    if size.is_finite() {
        size.clamp(6.0, 72.0)
    } else {
        DEFAULT_FONT_SIZE
    }
}

pub fn clamp_agent_transcript_font_size(size: f64) -> f64 {
    if size.is_finite() {
        size.clamp(6.0, 72.0)
    } else {
        DEFAULT_AGENT_TRANSCRIPT_FONT_SIZE
    }
}

pub fn clamp_terminal_line_height(line_height: f64) -> f64 {
    if line_height.is_finite() {
        line_height.clamp(0.8, 3.0)
    } else {
        DEFAULT_LINE_HEIGHT
    }
}

/// Clamp a persisted opacity into `min..=1.0`; non-finite values (a hand-
/// edited config) fall back to `fallback`.
fn clamp_opacity(opacity: f64, min: f64, fallback: f64) -> f64 {
    if opacity.is_finite() {
        opacity.clamp(min, 1.0)
    } else {
        fallback
    }
}

/// The 0.2 floor keeps the window from becoming effectively invisible.
pub fn clamp_background_opacity(opacity: f64) -> f64 {
    clamp_opacity(opacity, 0.2, 1.0)
}

pub fn clamp_background_image_opacity(opacity: f64) -> f64 {
    clamp_opacity(opacity, 0.0, DEFAULT_BACKGROUND_IMAGE_OPACITY)
}

impl AppearanceConfig {
    pub fn normalize(&mut self) {
        self.git_status_refresh_interval = clamp_git_interval(self.git_status_refresh_interval);

        self.ui_font = ui_font_or_default(&self.ui_font);

        self.terminal_font_family = terminal_font_or_default(&self.terminal_font_family);

        self.agent_font_family = ui_font_or_default(&self.agent_font_family);

        self.agent_transcript_font_family =
            terminal_font_or_default(&self.agent_transcript_font_family);

        self.terminal_font_size = clamp_terminal_font_size(self.terminal_font_size);
        self.agent_font_size = clamp_terminal_font_size(self.agent_font_size);

        self.agent_transcript_font_size =
            clamp_agent_transcript_font_size(self.agent_transcript_font_size);

        self.terminal_line_height = clamp_terminal_line_height(self.terminal_line_height);

        self.tab_width = clamp_tab_width(self.tab_width);
        self.background_opacity = clamp_background_opacity(self.background_opacity);

        self.background_image_opacity =
            clamp_background_image_opacity(self.background_image_opacity);

        self.background_image = self
            .background_image
            .take()
            .filter(|path| !path.trim().is_empty());
    }
}
