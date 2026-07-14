pub mod agent;
pub mod appearance;
pub mod bell;
pub mod bindings;
pub mod colors;
pub mod defaults;
pub mod effects;
pub mod hints;
pub mod keyboard;
pub mod layout;
pub mod local_state;
pub mod navigation;
pub mod platform;
pub mod profile;
pub mod render_types;
pub mod renderer;
pub mod system;
pub mod theme;
pub mod title;
pub mod window;

use std::default::Default;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{OnceLock, RwLock};

use colors::Colors;
use serde::{Deserialize, Serialize};
use theme::{AdaptiveColors, AdaptiveTheme, AppearanceTheme, Theme, UiTheme};
use tracing::warn;

use crate::bell::Bell;
use crate::bindings::Bindings;
use crate::defaults::*;
use crate::hints::Hints;
use crate::keyboard::Keyboard;
use crate::layout::{Margin, Panel};
use crate::navigation::Navigation;
use crate::platform::{Platform, PlatformConfig};
use crate::renderer::Renderer;
use crate::title::Title;
use crate::window::Window;

#[derive(Clone, Debug)]
pub enum ConfigError {
    ErrLoadingConfig(String),
    ErrLoadingTheme(String),
    PathNotFound,
}

#[derive(Default, Debug, Serialize, Deserialize, PartialEq, Clone)]
pub struct Shell {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
pub struct Scroll {
    pub multiplier: f64,
    pub divider: f64,
}

impl Default for Scroll {
    fn default() -> Scroll {
        Scroll {
            multiplier: 3.0,
            divider: 1.0,
        }
    }
}

#[derive(Debug, PartialEq, Clone, Serialize, Deserialize)]
pub struct Developer {
    #[serde(default = "bool::default", rename = "enable-fps-counter")]
    pub enable_fps_counter: bool,
    #[serde(default = "default_log_level", rename = "log-level")]
    pub log_level: String,
    #[serde(rename = "enable-log-file", default)]
    pub enable_log_file: bool,
}

impl Default for Developer {
    fn default() -> Developer {
        Developer {
            log_level: default_log_level(),
            enable_log_file: false,
            enable_fps_counter: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Config {
    #[serde(default)]
    pub cursor: CursorConfig,
    #[serde(default = "Navigation::default")]
    pub navigation: Navigation,
    #[serde(default = "Window::default")]
    pub window: Window,
    #[serde(default = "default_shell")]
    pub shell: Shell,
    #[serde(default = "Platform::default")]
    pub platform: Platform,
    #[serde(default = "default_use_fork", rename = "use-fork")]
    pub use_fork: bool,
    #[serde(default = "Keyboard::default")]
    pub keyboard: Keyboard,
    #[serde(default = "Title::default")]
    pub title: Title,
    #[serde(default = "default_working_dir", rename = "working-dir")]
    pub working_dir: Option<String>,
    #[serde(default = "default_theme")]
    pub theme: String,
    #[serde(default = "Scroll::default")]
    pub scroll: Scroll,
    #[serde(
        default = "Option::default",
        skip_serializing,
        rename = "adaptive-theme"
    )]
    pub adaptive_theme: Option<AdaptiveTheme>,
    #[serde(default = "default_editor")]
    pub editor: Shell,
    #[serde(default = "default_margin", alias = "margin")]
    pub margin: Margin,
    #[serde(default = "Panel::default")]
    pub panel: Panel,
    #[serde(default = "Vec::default", rename = "env-vars")]
    pub env_vars: Vec<String>,
    #[serde(default = "default_option_as_alt", rename = "option-as-alt")]
    pub option_as_alt: String,
    #[serde(skip)]
    pub colors: Colors,
    /// UI theme loaded from the selected file in `themes/`.
    #[serde(skip)]
    pub ui_theme: Option<UiTheme>,
    #[serde(default = "Option::default", skip_serializing)]
    pub adaptive_colors: Option<AdaptiveColors>,
    #[serde(default = "Option::default", rename = "force-theme")]
    pub force_theme: Option<AppearanceTheme>,
    #[serde(default = "Developer::default")]
    pub developer: Developer,
    #[serde(default = "Bindings::default")]
    pub bindings: bindings::Bindings,
    #[serde(
        default = "bool::default",
        rename = "ignore-selection-foreground-color"
    )]
    pub ignore_selection_fg_color: bool,
    #[serde(default = "default_bool_true", rename = "confirm-before-quit")]
    pub confirm_before_quit: bool,
    #[serde(default = "bool::default", rename = "copy-on-select")]
    pub copy_on_select: bool,
    #[serde(
        default = "bool::default",
        rename = "hide-mouse-cursor-when-typing",
        alias = "hide-cursor-when-typing"
    )]
    pub hide_cursor_when_typing: bool,
    #[serde(default = "Renderer::default")]
    pub renderer: Renderer,
    #[serde(default = "bool::default", rename = "draw-bold-text-with-light-colors")]
    pub draw_bold_text_with_light_colors: bool,
    #[serde(default = "Hints::default")]
    pub hints: Hints,
    #[serde(default = "Bell::default")]
    pub bell: Bell,
    #[serde(default = "default_bool_true", rename = "enable-scroll-bar")]
    pub enable_scroll_bar: bool,
    #[serde(
        default = "default_scrollback_history_limit",
        rename = "scrollback-history-limit"
    )]
    pub scrollback_history_limit: usize,
    #[serde(default = "effects::Effects::default")]
    pub effects: effects::Effects,
    /// Visual settings (settings dialog, Terminal/Appearance pages).
    #[serde(default = "appearance::AppearanceConfig::default")]
    pub appearance: appearance::AppearanceConfig,
    /// The `[profiles]` section: default-profile name + profile entries.
    #[serde(default)]
    pub profiles: profile::ProfilesConfig,
    /// Agent integration settings (settings dialog, Agent page).
    #[serde(default = "agent::AgentConfig::default")]
    pub agent: agent::AgentConfig,
    /// System-behavior settings (settings dialog, System page).
    #[serde(default = "system::SystemConfig::default")]
    pub system: system::SystemConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CursorConfig {
    #[serde(default = "default_cursor")]
    pub shape: CursorShape,
    #[serde(default = "bool::default")]
    pub blinking: bool,
    #[serde(default = "default_cursor_interval", rename = "blinking-interval")]
    pub blinking_interval: u64,
}

static TESTING_MODE: AtomicBool = AtomicBool::new(false);

/// Select the isolated `Test` configuration directory before configuration is loaded.
pub fn enable_testing_mode() {
    TESTING_MODE.store(true, Ordering::Relaxed);
}

fn config_dir_for_mode(path: PathBuf, testing: bool) -> PathBuf {
    if testing { path.join("Test") } else { path }
}

fn selected_config_dir(path: PathBuf) -> PathBuf {
    config_dir_for_mode(path, TESTING_MODE.load(Ordering::Relaxed))
}

#[cfg(target_os = "macos")]
#[inline]
fn base_config_dir_path() -> PathBuf {
    std::env::var("NMT_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or(dirs::home_dir().unwrap().join(".config").join("NiumaTerm"))
}

#[cfg(target_os = "windows")]
#[inline]
fn base_config_dir_path() -> PathBuf {
    std::env::var("NMT_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or(
            dirs::home_dir()
                .unwrap()
                .join("AppData")
                .join("Local")
                .join("NiumaTerm"),
        )
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
#[inline]
fn base_config_dir_path() -> PathBuf {
    std::env::var("NMT_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or(
            std::env::var("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .unwrap_or(dirs::home_dir().unwrap().join(".config"))
                .join("NiumaTerm"),
        )
}

#[inline]
pub fn config_dir_path() -> PathBuf {
    selected_config_dir(base_config_dir_path())
}

#[inline]
pub fn config_file_path() -> PathBuf {
    config_dir_path().join("config.toml")
}

fn theme_file_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.toml"))
}

impl Config {
    #[cfg(test)]
    fn load_from_path(path: &PathBuf) -> Self {
        if path.exists() {
            let content = std::fs::read_to_string(path).unwrap();
            let decoded: Config = toml::from_str(&content).unwrap_or_else(|_| Config::default());
            decoded
        } else {
            Config::default()
        }
    }
    #[cfg(test)]
    fn load_from_path_without_fallback(path: &PathBuf) -> Result<Self, String> {
        if path.exists() {
            let content = std::fs::read_to_string(path).unwrap();
            match toml::from_str::<Config>(&content) {
                Ok(mut decoded) => {
                    let theme = &decoded.theme;
                    if theme.is_empty() {
                        return Ok(decoded);
                    }

                    let tmp = std::env::temp_dir();
                    let path = theme_file_path(&tmp, theme);
                    if let Ok(loaded_theme) = Config::load_theme(&path) {
                        decoded.ui_theme = loaded_theme.ui_theme();
                        decoded.colors = loaded_theme.colors.terminal;
                    } else {
                        warn!("failed to load theme: {}", theme);
                    }

                    if let Some(adaptive_theme) = &decoded.adaptive_theme {
                        let light_theme = &adaptive_theme.light;
                        let path = theme_file_path(&tmp, light_theme);
                        let mut adaptive_colors = AdaptiveColors {
                            dark: None,
                            light: None,
                        };

                        if let Ok(light_loaded_theme) = Config::load_theme(&path) {
                            adaptive_colors.light = Some(light_loaded_theme.colors.terminal);
                        } else {
                            warn!("failed to load light theme: {}", light_theme);
                        }

                        let dark_theme = &adaptive_theme.dark;
                        let path = theme_file_path(&tmp, dark_theme);
                        if let Ok(dark_loaded_theme) = Config::load_theme(&path) {
                            adaptive_colors.dark = Some(dark_loaded_theme.colors.terminal);
                        } else {
                            warn!("failed to load dark theme: {}", dark_theme);
                        }

                        if adaptive_colors.light.is_some() && adaptive_colors.dark.is_some() {
                            decoded.adaptive_colors = Some(adaptive_colors);
                        }
                    }

                    Ok(decoded)
                }
                Err(err_message) => Err(format!("error parsing: {err_message:?}")),
            }
        } else {
            Err(String::from("filepath does not exist"))
        }
    }

    fn load_theme(path: &PathBuf) -> Result<Theme, String> {
        let content = if path.exists() {
            std::fs::read_to_string(path).map_err(|err| err.to_string())?
        } else {
            let name = path
                .file_stem()
                .and_then(|name| name.to_str())
                .ok_or_else(|| String::from("invalid theme filepath"))?;
            nmt_themes::get(name)
                .map(str::to_owned)
                .ok_or_else(|| String::from("filepath does not exist"))?
        };
        toml::from_str::<Theme>(&content)
            .map_err(|err_message| format!("error parsing: {err_message:?}"))
    }

    /// Load a named theme from the per-user `themes` directory.
    pub fn load_named_theme(name: &str) -> Result<Theme, String> {
        let path = Path::new(name);
        if path.file_name().and_then(|name| name.to_str()) != Some(name) {
            return Err(String::from("theme name must not contain a path"));
        }
        Self::load_theme(&theme_file_path(&config_dir_path().join("themes"), name))
    }

    /// Load every valid `.toml` theme in the per-user themes directory.
    pub fn load_themes() -> Vec<(String, Theme)> {
        let mut themes = nmt_themes::THEMES
            .iter()
            .filter_map(|builtin| match toml::from_str::<Theme>(builtin.source) {
                Ok(theme) => Some((builtin.name.to_string(), theme)),
                Err(err) => {
                    warn!("ignored invalid built-in theme {}: {err}", builtin.name);
                    None
                }
            })
            .collect::<Vec<_>>();
        for custom in Self::load_themes_from(&config_dir_path().join("themes")) {
            merge_theme(&mut themes, custom);
        }
        themes.sort_by_key(|(name, _)| name.to_lowercase());
        themes
    }

    fn load_themes_from(path: &Path) -> Vec<(String, Theme)> {
        let Ok(entries) = std::fs::read_dir(path) else {
            return Vec::new();
        };
        let mut themes = entries
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().and_then(|ext| ext.to_str()) == Some("toml"))
            .filter_map(|entry| {
                let path = entry.path();
                let name = path.file_stem()?.to_str()?.to_string();
                match Self::load_theme(&path) {
                    Ok(theme) => Some((name, theme)),
                    Err(err) => {
                        warn!("ignored invalid theme {}: {err}", path.display());
                        None
                    }
                }
            })
            .collect::<Vec<_>>();
        themes.sort_by_key(|(name, _)| name.to_lowercase());
        themes
    }

    pub fn to_string(&self) -> Result<String, toml::ser::Error> {
        toml::to_string(self)
    }

    pub fn load() -> Self {
        let config_path = config_dir_path();
        let path = config_file_path();
        if path.exists() {
            let content = std::fs::read_to_string(path).unwrap();
            match toml::from_str::<Config>(&content) {
                Ok(mut decoded) => {
                    let theme = &decoded.theme;
                    if theme.is_empty() {
                        return decoded;
                    }

                    let path = theme_file_path(&config_path.join("themes"), theme);
                    if let Ok(loaded_theme) = Config::load_theme(&path) {
                        decoded.ui_theme = loaded_theme.ui_theme();
                        decoded.colors = loaded_theme.colors.terminal;
                    } else {
                        warn!("failed to load theme: {}", theme);
                    }

                    decoded
                }
                Err(err_message) => {
                    warn!(
                        "failure to parse config file, falling back to default...\n{err_message:?}"
                    );
                    Config::default()
                }
            }
        } else {
            Config::default()
        }
    }

    pub fn load_for_startup() -> Result<Self, toml::de::Error> {
        Config::load_for_startup_from(&config_file_path(), &config_dir_path())
    }

    fn load_for_startup_from(path: &Path, config_dir: &Path) -> Result<Self, toml::de::Error> {
        let Some(content) = std::fs::read_to_string(path).ok() else {
            return Ok(Config::default());
        };
        let mut decoded = toml::from_str::<Config>(&content)?;
        let theme = &decoded.theme;
        if !theme.is_empty() {
            let path = theme_file_path(&config_dir.join("themes"), theme);
            if let Ok(loaded_theme) = Config::load_theme(&path) {
                decoded.ui_theme = loaded_theme.ui_theme();
                decoded.colors = loaded_theme.colors.terminal;
            } else {
                warn!("failed to load theme: {}", theme);
            }
        }

        Ok(decoded)
    }

    pub fn try_load() -> Result<Self, ConfigError> {
        let path = config_file_path();
        if path.exists() {
            match std::fs::read_to_string(path) {
                Ok(content) => match toml::from_str::<Config>(&content) {
                    Ok(mut decoded) => {
                        let theme = &decoded.theme;
                        let theme_path = config_dir_path().join("themes");
                        if !theme.is_empty() {
                            let path = theme_file_path(&theme_path, theme);
                            match Config::load_theme(&path) {
                                Ok(loaded_theme) => {
                                    decoded.ui_theme = loaded_theme.ui_theme();
                                    decoded.colors = loaded_theme.colors.terminal;
                                }
                                Err(err_message) => {
                                    return Err(ConfigError::ErrLoadingTheme(err_message));
                                }
                            }
                        }

                        if let Some(adaptive_theme) = &decoded.adaptive_theme {
                            let mut adaptive_colors = AdaptiveColors {
                                dark: None,
                                light: None,
                            };

                            let light_theme = &adaptive_theme.light;
                            let path = theme_file_path(&theme_path, light_theme);
                            match Config::load_theme(&path) {
                                Ok(light_loaded_theme) => {
                                    adaptive_colors.light = Some(light_loaded_theme.colors.terminal)
                                }
                                Err(err_message) => {
                                    warn!("failed to load light theme: {}", light_theme);
                                    return Err(ConfigError::ErrLoadingTheme(err_message));
                                }
                            }

                            let dark_theme = &adaptive_theme.dark;
                            let path = theme_file_path(&theme_path, dark_theme);
                            match Config::load_theme(&path) {
                                Ok(dark_loaded_theme) => {
                                    adaptive_colors.dark = Some(dark_loaded_theme.colors.terminal)
                                }
                                Err(err_message) => {
                                    warn!("failed to load dark theme: {}", dark_theme);
                                    return Err(ConfigError::ErrLoadingTheme(err_message));
                                }
                            }

                            if adaptive_colors.light.is_some() && adaptive_colors.dark.is_some() {
                                decoded.adaptive_colors = Some(adaptive_colors);
                            }
                        }

                        Ok(decoded)
                    }
                    Err(err_message) => Err(ConfigError::ErrLoadingConfig(err_message.to_string())),
                },
                Err(err_message) => Err(ConfigError::ErrLoadingConfig(err_message.to_string())),
            }
        } else {
            Err(ConfigError::PathNotFound)
        }
    }

    pub fn overwrite_based_on_platform(&mut self) {
        #[cfg(windows)]
        if let Some(windows) = &self.platform.windows {
            self.overwrite_with_platform_config(windows.clone());
        }

        #[cfg(target_os = "linux")]
        if let Some(linux) = &self.platform.linux {
            self.overwrite_with_platform_config(linux.clone());
        }

        #[cfg(target_os = "macos")]
        if let Some(macos) = &self.platform.macos {
            self.overwrite_with_platform_config(macos.clone());
        }
    }

    fn overwrite_with_platform_config(&mut self, platform_config: PlatformConfig) {
        // Replace shell entirely if specified
        if let Some(shell_overwrite) = &platform_config.shell {
            self.shell = shell_overwrite.clone();
        }

        // Merge window fields individually
        if let Some(window_overwrite) = &platform_config.window {
            if let Some(width) = window_overwrite.width {
                self.window.width = width;
            }
            if let Some(height) = window_overwrite.height {
                self.window.height = height;
            }
            if let Some(columns) = window_overwrite.columns {
                self.window.columns = Some(columns);
            }
            if let Some(rows) = window_overwrite.rows {
                self.window.rows = Some(rows);
            }
            if let Some(mode) = window_overwrite.mode {
                self.window.mode = mode;
            }
            if let Some(opacity) = window_overwrite.opacity {
                self.window.opacity = opacity;
            }
            if let Some(blur) = window_overwrite.blur {
                self.window.blur = blur;
            }
            if let Some(bg_image) = &window_overwrite.background_image {
                self.window.background_image = Some(bg_image.clone());
            }
            if let Some(decorations) = window_overwrite.decorations {
                self.window.decorations = decorations;
            }
            if let Some(macos_unified) = window_overwrite.macos_use_unified_titlebar {
                self.window.macos_use_unified_titlebar = macos_unified;
            }
            if let Some(macos_shadow) = window_overwrite.macos_use_shadow {
                self.window.macos_use_shadow = macos_shadow;
            }
            if let Some(x) = window_overwrite.macos_traffic_light_position_x {
                self.window.macos_traffic_light_position_x = Some(x);
            }
            if let Some(y) = window_overwrite.macos_traffic_light_position_y {
                self.window.macos_traffic_light_position_y = Some(y);
            }
            if let Some(initial_title) = &window_overwrite.initial_title {
                self.window.initial_title = Some(initial_title.clone());
            }
            if let Some(win_shadow) = window_overwrite.windows_use_undecorated_shadow {
                self.window.windows_use_undecorated_shadow = Some(win_shadow);
            }
            if let Some(win_bitmap) = window_overwrite.windows_use_no_redirection_bitmap {
                self.window.windows_use_no_redirection_bitmap = Some(win_bitmap);
            }
            if let Some(win_corner) = &window_overwrite.windows_corner_preference {
                self.window.windows_corner_preference = Some(win_corner.clone());
            }
            if let Some(colorspace) = window_overwrite.colorspace {
                self.window.colorspace = colorspace;
            }
        }

        // Merge navigation fields individually
        if let Some(navigation_overwrite) = &platform_config.navigation {
            if let Some(mode) = navigation_overwrite.mode {
                self.navigation.mode = mode;
            }
            if let Some(color_automation) = &navigation_overwrite.color_automation {
                self.navigation.color_automation = color_automation.clone();
            }
            if let Some(clickable) = navigation_overwrite.clickable {
                self.navigation.clickable = clickable;
            }
            if let Some(cwd) = navigation_overwrite.current_working_directory {
                self.navigation.current_working_directory = cwd;
            }
            if let Some(use_term_title) = navigation_overwrite.use_terminal_title {
                self.navigation.use_terminal_title = use_term_title;
            }
            if let Some(hide_if_single) = navigation_overwrite.hide_if_single {
                self.navigation.hide_if_single = hide_if_single;
            }
            if let Some(use_split) = navigation_overwrite.use_split {
                self.navigation.use_split = use_split;
            }
            if let Some(open_cfg_split) = navigation_overwrite.open_config_with_split {
                self.navigation.open_config_with_split = open_cfg_split;
            }
            if let Some(unfocused_opacity) = navigation_overwrite.unfocused_split_opacity {
                self.navigation.unfocused_split_opacity = unfocused_opacity;
            }
            if let Some(fill) = navigation_overwrite.unfocused_split_fill {
                self.navigation.unfocused_split_fill = Some(fill);
            }
        }

        // Clamp after platform merge so both the base and any override go
        // through the same bound.
        self.navigation.unfocused_split_opacity = crate::navigation::clamp_unfocused_split_opacity(
            self.navigation.unfocused_split_opacity,
        );

        // Merge renderer fields individually
        if let Some(renderer_overwrite) = &platform_config.renderer {
            if let Some(backend) = &renderer_overwrite.backend {
                self.renderer.backend = backend.clone();
            }
            if let Some(disable_unfocused) = renderer_overwrite.disable_unfocused_render {
                self.renderer.disable_unfocused_render = disable_unfocused;
            }
            if let Some(disable_occluded) = renderer_overwrite.disable_occluded_render {
                self.renderer.disable_occluded_render = disable_occluded;
            }
            if let Some(strategy) = &renderer_overwrite.strategy {
                self.renderer.strategy = strategy.clone();
            }
        }

        // Append platform-specific env vars to the global ones
        if let Some(env_vars_overwrite) = &platform_config.env_vars {
            self.env_vars.extend(env_vars_overwrite.clone());
        }

        // Override theme
        if let Some(theme_overwrite) = &platform_config.theme {
            self.theme = theme_overwrite.clone();
        }
    }
}

fn merge_theme(themes: &mut Vec<(String, Theme)>, custom: (String, Theme)) {
    if let Some(existing) = themes
        .iter_mut()
        .find(|(name, _)| name.eq_ignore_ascii_case(&custom.0))
    {
        *existing = custom;
    } else {
        themes.push(custom);
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            cursor: CursorConfig::default(),
            editor: default_editor(),
            adaptive_theme: None,
            adaptive_colors: None,
            force_theme: None,
            bindings: Bindings::default(),
            colors: Colors::default(),
            ui_theme: None,
            scroll: Scroll::default(),
            keyboard: Keyboard::default(),
            title: Title::default(),
            developer: Developer::default(),
            env_vars: vec![],
            navigation: Navigation::default(),
            option_as_alt: default_option_as_alt(),
            margin: default_margin(),
            panel: Panel::default(),
            renderer: Renderer::default(),
            shell: default_shell(),
            platform: Platform::default(),
            theme: default_theme(),
            use_fork: default_use_fork(),
            window: Window::default(),
            working_dir: default_working_dir(),
            ignore_selection_fg_color: false,
            confirm_before_quit: true,
            copy_on_select: false,
            hide_cursor_when_typing: false,
            draw_bold_text_with_light_colors: false,
            hints: Hints::default(),
            bell: Bell::default(),
            enable_scroll_bar: true,
            scrollback_history_limit: default_scrollback_history_limit(),
            effects: effects::Effects::default(),
            appearance: appearance::AppearanceConfig::default(),
            profiles: profile::ProfilesConfig::default(),
            agent: agent::AgentConfig::default(),
            system: system::SystemConfig::default(),
        }
    }
}

impl Default for CursorConfig {
    fn default() -> Self {
        Self {
            shape: default_cursor(),
            blinking: false,
            blinking_interval: default_cursor_interval(),
        }
    }
}

/// Cursor shape. Lives here (not in `nmt_terminal::ansi`) because it is a config
/// value `terminal` deserializes; `terminal` re-exports it as `ansi::CursorShape`.
#[derive(Default, Clone, Serialize, Deserialize, Copy, Debug, Eq, PartialEq)]
pub enum CursorShape {
    /// Cursor is a block like `▒`.
    #[default]
    #[serde(alias = "block")]
    Block,
    /// Cursor is an underscore like `_`.
    #[serde(alias = "underline")]
    Underline,
    /// Cursor is a vertical bar `⎸`.
    #[serde(alias = "beam")]
    Beam,
    /// Cursor is hidden.
    #[serde(alias = "hidden")]
    Hidden,
}

impl CursorShape {
    pub fn from_char(c: char) -> CursorShape {
        match c {
            '_' => CursorShape::Underline,
            '|' => CursorShape::Beam,
            _ => CursorShape::Block,
        }
    }
}

impl From<CursorShape> for char {
    fn from(value: CursorShape) -> Self {
        match value {
            CursorShape::Underline => '_',
            CursorShape::Beam => '|',
            _ => '▇',
        }
    }
}

static CONFIG: OnceLock<Config> = OnceLock::new();
static ACTIVE_COLORS: OnceLock<RwLock<Colors>> = OnceLock::new();

/// Install `config` as the process-wide config. Ignored if already initialized.
pub fn init(config: Config) {
    set_active_colors(config.colors);
    let _ = CONFIG.set(config);
}

/// Load the config file from disk and install it as the global, returning a
/// reference. Call once during startup.
pub fn init_from_file() -> &'static Config {
    CONFIG.get_or_init(Config::load)
}

/// The global config, falling back to defaults if never initialized.
pub fn get() -> &'static Config {
    CONFIG.get_or_init(Config::default)
}

/// Return the active terminal palette. Unlike the rest of the startup config,
/// this value can change when the user selects a theme.
pub fn active_colors() -> Colors {
    *ACTIVE_COLORS
        .get_or_init(|| RwLock::new(get().colors))
        .read()
        .expect("active theme colors lock poisoned")
}

pub fn set_active_colors(colors: Colors) {
    *ACTIVE_COLORS
        .get_or_init(|| RwLock::new(colors))
        .write()
        .expect("active theme colors lock poisoned") = colors;
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use colors::hex_to_color_arr;

    use super::*;

    #[test]
    fn testing_mode_uses_test_subdirectory() {
        let base = PathBuf::from("NiumaTerm");
        assert_eq!(config_dir_for_mode(base.clone(), false), base);
        assert_eq!(config_dir_for_mode(base.clone(), true), base.join("Test"));
    }

    fn tmp_dir() -> PathBuf {
        std::env::temp_dir()
    }

    fn create_temporary_config(prefix: &str, toml_str: &str) -> Config {
        let file_name = tmp_dir().join(format!("test-rio-{prefix}-config.toml"));
        let mut file = std::fs::File::create(&file_name).unwrap();
        writeln!(file, "{toml_str}").unwrap();

        match Config::load_from_path_without_fallback(&file_name) {
            Ok(config) => config,
            Err(e) => panic!("{e}"),
        }
    }

    fn create_temporary_theme(theme: &str, toml_str: &str) {
        let file_name = tmp_dir().join(theme).with_extension("toml");
        let mut file = std::fs::File::create(file_name).unwrap();
        writeln!(file, "{toml_str}").unwrap();
    }

    #[test]
    fn test_filepath_does_not_exist_without_fallback() {
        let should_fail =
            Config::load_from_path_without_fallback(&tmp_dir().join("it-should-never-exist"));
        assert!(should_fail.is_err(), "{}", true);
    }

    #[test]
    fn test_filepath_does_not_exist_with_fallback() {
        let config = Config::load_from_path(&tmp_dir().join("it-should-never-exist"));
        assert_eq!(config.theme, default_theme());
        assert_eq!(config.cursor.shape, default_cursor());
    }

    #[test]
    fn startup_load_defaults_when_missing_and_errors_on_bad_toml() {
        let dir = tmp_dir().join("NiumaTerm-startup-config-test");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("config.toml");

        let missing = Config::load_for_startup_from(&path, &dir).unwrap();
        assert_eq!(missing, Config::default());

        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, "not [ valid").unwrap();
        assert!(Config::load_for_startup_from(&path, &dir).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_if_explicit_defaults_match() {
        // An empty config file must resolve to the explicit defaults.
        let result = create_temporary_config("defaults", "");

        let env_vars: Vec<String> = vec![];
        assert_eq!(result.env_vars, env_vars);
        assert_eq!(result.cursor.shape, default_cursor());
        assert_eq!(result.theme, default_theme());
        assert_eq!(result.cursor.shape, default_cursor());
        assert_eq!(result.shell, default_shell());
        assert!(!result.renderer.disable_unfocused_render);
        assert_eq!(result.use_fork, default_use_fork());

        // Colors
        assert_eq!(result.colors, Colors::default());
        // Developer
        assert_eq!(result.developer, Developer::default());
        assert_eq!(result.bindings, Bindings::default());
    }

    #[test]
    fn test_invalid_config_file() {
        let toml_str = r#"
            Performance = 2
            width = "big"
            height = "small"
        "#;

        let file_name = tmp_dir()
            .join("test-rio-invalid-config")
            .with_extension("toml");
        let mut file = std::fs::File::create(&file_name).unwrap();
        writeln!(file, "{toml_str}").unwrap();

        let result = Config::load_from_path(&file_name);

        assert_eq!(result.theme, default_theme());
        // Colors
        assert_eq!(result.colors.background, colors::defaults::background());
        assert_eq!(result.colors.foreground, colors::defaults::foreground());
        assert_eq!(result.colors.tabs_active, colors::defaults::tabs_active());
        assert_eq!(result.colors.cursor, colors::defaults::cursor());
    }

    #[test]
    fn test_change_config_renderer() {
        let result = create_temporary_config(
            "change-performance",
            r#"
            [renderer]
            performance = "Low"
            backend = "Cpu"
        "#,
        );

        assert_eq!(result.renderer.backend, crate::renderer::Backend::Cpu);
        assert_eq!(result.theme, default_theme());
        // Colors
        assert_eq!(result.colors.background, colors::defaults::background());
        assert_eq!(result.colors.foreground, colors::defaults::foreground());
        assert_eq!(result.colors.tabs_active, colors::defaults::tabs_active());
        assert_eq!(result.colors.cursor, colors::defaults::cursor());
    }

    #[test]
    fn test_change_config_environment_variables() {
        let result = create_temporary_config(
            "change-env-vars",
            r#"
            env-vars = ['A=5', 'B=8']
        "#,
        );

        assert_eq!(result.env_vars, [String::from("A=5"), String::from("B=8")]);
        assert_eq!(result.cursor.shape, default_cursor());
        assert_eq!(result.theme, default_theme());
        // Colors
        assert_eq!(result.colors.background, colors::defaults::background());
        assert_eq!(result.colors.foreground, colors::defaults::foreground());
        assert_eq!(result.colors.tabs_active, colors::defaults::tabs_active());
        assert_eq!(
            result.colors.selection_background,
            colors::defaults::selection_background()
        );
        assert_eq!(
            result.colors.selection_foreground,
            colors::defaults::selection_foreground()
        );
        assert_eq!(result.colors.cursor, colors::defaults::cursor());
    }

    #[test]
    fn test_change_config_cursor() {
        let result = create_temporary_config(
            "change-cursor",
            r#"
            [cursor]
            shape = 'underline'
        "#,
        );

        assert_eq!(result.cursor.shape, CursorShape::Underline);
        assert_eq!(result.theme, default_theme());
        // Colors
        assert_eq!(result.colors.background, colors::defaults::background());
        assert_eq!(result.colors.foreground, colors::defaults::foreground());
        assert_eq!(result.colors.tabs_active, colors::defaults::tabs_active());
        assert_eq!(result.colors.cursor, colors::defaults::cursor());
    }

    #[test]
    fn test_change_option_as_alt() {
        let result = create_temporary_config(
            "change-option-as-alt",
            r#"
            option-as-alt = 'Both'
        "#,
        );

        assert_eq!(result.option_as_alt, String::from("Both"));
        assert_eq!(result.theme, default_theme());
        // Colors
        assert_eq!(result.colors.background, colors::defaults::background());
        assert_eq!(result.colors.foreground, colors::defaults::foreground());
        assert_eq!(result.colors.tabs_active, colors::defaults::tabs_active());
        assert_eq!(result.colors.cursor, colors::defaults::cursor());
    }

    #[test]
    fn test_change_bindings() {
        let result = create_temporary_config(
            "change-key-bindings",
            r#"
            [bindings]
            keys = [
                { key = 'Q', with = 'super', action = 'Quit' }
            ]
        "#,
        );

        assert_eq!(result.theme, default_theme());
        // Bindings
        assert_eq!(result.bindings.keys[0].key, "Q");
        assert_eq!(result.bindings.keys[0].with, "super");
        assert_eq!(result.bindings.keys[0].action.to_owned(), "Quit");
        assert!(result.bindings.keys[0].esc.to_owned().is_empty());
    }

    #[test]
    fn test_change_style() {
        let result = create_temporary_config(
            "change-style",
            r#"
            font-size = 14.0
            margin = [0]

            [renderer]
            performance = "Low"

            [window]
            opacity = 0.5
            [window.background-image]
            path = "my-image-path.png"

        "#,
        );

        assert_eq!(result.margin.top, 0.0);
        assert_eq!(result.margin.bottom, 0.0);
        assert_eq!(result.margin.left, 0.0);
        assert_eq!(result.margin.right, 0.0);
        assert_eq!(result.window.opacity, 0.5);
        assert_eq!(
            result.window.background_image,
            Some(crate::render_types::ImageProperties {
                path: String::from("my-image-path.png"),
                ..crate::render_types::ImageProperties::default()
            })
        );
        // Colors
        assert_eq!(result.colors.background, colors::defaults::background());
        assert_eq!(result.colors.foreground, colors::defaults::foreground());
        assert_eq!(result.colors.tabs_active, colors::defaults::tabs_active());
        assert_eq!(result.colors.cursor, colors::defaults::cursor());
    }

    #[test]
    fn test_change_theme() {
        let result = create_temporary_config(
            "change-theme",
            r#"
            theme = "lucario"
        "#,
        );

        assert_eq!(result.theme, "lucario");
        // Colors
        assert_eq!(result.colors.background, colors::defaults::background());
        assert_eq!(result.colors.foreground, colors::defaults::foreground());
        assert_eq!(result.colors.tabs_active, colors::defaults::tabs_active());
        assert_eq!(result.colors.cursor, colors::defaults::cursor());
    }

    #[test]
    fn test_change_theme_with_colors() {
        create_temporary_theme(
            "lucario-with-colors",
            r#"
            name = 'Lucario'
            mode = 'dark'

            [colors.terminal]
            background       = '#2B3E50'
            foreground       = '#F8F8F2'

            [colors.ui]
            background = '#2B3E50'
        "#,
        );

        let result = create_temporary_config(
            "change-theme-with-colors",
            r#"
            theme = "lucario-with-colors"
        "#,
        );

        // Colors
        assert_eq!(result.colors.tabs_active, colors::defaults::tabs_active());
        assert_eq!(result.colors.cursor, colors::defaults::cursor());
        assert_eq!(result.colors.foreground, hex_to_color_arr("#F8F8F2"));
        assert_eq!(result.colors.background.0, hex_to_color_arr("#2B3E50"));
        assert_eq!(result.ui_theme.as_ref().unwrap().name, "Lucario");
        assert_eq!(
            result.ui_theme.as_ref().unwrap().mode,
            AppearanceTheme::Dark
        );
        assert_eq!(
            result.ui_theme.as_ref().unwrap().colors["background"].as_str(),
            Some("#2B3E50")
        );
    }

    #[test]
    fn theme_list_loads_valid_toml_files_in_name_order() {
        let dir = std::env::temp_dir().join("NiumaTerm-theme-list-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("Zulu.toml"),
            "[colors.terminal]\nbackground = '#111111'\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("alpha.toml"),
            "[colors.terminal]\nbackground = '#222222'\n",
        )
        .unwrap();
        std::fs::write(dir.join("invalid.toml"), "[colors\n").unwrap();
        std::fs::write(dir.join("ignored.txt"), "[colors.terminal]\n").unwrap();

        let themes = Config::load_themes_from(&dir);
        assert_eq!(
            themes
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            ["alpha", "Zulu"]
        );

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn built_in_themes_load_without_user_files() {
        for builtin in nmt_themes::THEMES {
            let path = tmp_dir()
                .join("NiumaTerm-missing-builtins")
                .join(builtin.name)
                .with_extension("toml");
            let theme = Config::load_theme(&path).unwrap();
            assert!(!theme.name.is_empty());
        }
    }

    #[test]
    fn custom_theme_overrides_builtin_case_insensitively() {
        let mut themes = vec![(String::from("ubuntu"), Theme::default())];
        merge_theme(&mut themes, (String::from("Ubuntu"), Theme::default()));

        assert_eq!(themes.len(), 1);
        assert_eq!(themes[0].0, "Ubuntu");
    }

    #[test]
    fn top_level_colors_are_ignored() {
        let result = create_temporary_config(
            "ignored-colors",
            r#"
            theme = ""

            [colors]
            background = '#2B3E50'
        "#,
        );

        assert_eq!(result.colors, Colors::default());
    }

    #[test]
    fn test_use_fork() {
        let result = create_temporary_config(
            "change-use-fork",
            r#"
            use-fork = true

            [renderer]
            disable-unfocused-render = true
            performance = "Low"
        "#,
        );

        // Advanced
        assert!(result.renderer.disable_unfocused_render);
        assert!(result.use_fork);

        // Colors
        assert_eq!(result.colors.background, colors::defaults::background());
        assert_eq!(result.colors.foreground, colors::defaults::foreground());
        assert_eq!(result.colors.tabs_active, colors::defaults::tabs_active());
        assert_eq!(result.colors.cursor, colors::defaults::cursor());
    }

    #[test]
    fn test_shell() {
        let result = create_temporary_config(
            "change-shell-and-editor",
            r#"
            shell = { program = "/bin/fish", args = ["--hello"] }
        "#,
        );

        assert_eq!(result.shell.program, "/bin/fish");
        assert_eq!(result.shell.args, ["--hello"]);
    }

    #[test]
    fn test_shell_no_args() {
        let result = create_temporary_config(
            "change-shell-and-editor-no-args",
            r#"
            shell = { program = "/bin/fish" }
        "#,
        );

        assert_eq!(result.shell.program, "/bin/fish");
        assert_eq!(result.shell.args, Vec::<&str>::new());
    }

    #[test]
    fn test_change_developer_and_performance() {
        let result = create_temporary_config(
            "change-developer",
            r#"
            [renderer]
            performance = "Low"
            backend = "Cpu"

            [developer]
            enable-fps-counter = true
            log-level = "INFO"
        "#,
        );

        assert_eq!(result.renderer.backend, crate::renderer::Backend::Cpu);
        // Developer
        assert_eq!(result.developer.log_level, String::from("INFO"));
        assert!(result.developer.enable_fps_counter);

        // Colors
        assert_eq!(result.colors.background, colors::defaults::background());
        assert_eq!(result.colors.foreground, colors::defaults::foreground());
        assert_eq!(result.colors.tabs_active, colors::defaults::tabs_active());
        assert_eq!(result.colors.cursor, colors::defaults::cursor());
    }

    #[test]
    fn test_window_colorspace() {
        let result = create_temporary_config(
            "window-colorspace",
            r#"
            [window]
            colorspace = "display-p3"
        "#,
        );

        assert_eq!(result.window.colorspace, window::Colorspace::DisplayP3);
    }

    #[test]
    fn test_scrollback_history_limit_default() {
        let result = create_temporary_config(
            "scrollback-default",
            r#"
            [window]
            width = 800
        "#,
        );
        assert_eq!(result.scrollback_history_limit, 10_000);
    }

    #[test]
    fn test_scrollback_history_limit_zero_disables() {
        // A value of 0 disables scrollback. Must round-trip cleanly.
        let result = create_temporary_config(
            "scrollback-zero",
            r#"
            scrollback-history-limit = 0
        "#,
        );
        assert_eq!(result.scrollback_history_limit, 0);
    }

    #[test]
    fn test_window_colorspace_default() {
        let result = create_temporary_config(
            "window-colorspace-default",
            r#"
            [window]
            width = 800
            height = 600
        "#,
        );

        // Default is sRGB on every platform — same semantics as ghostty's
        // `window-colorspace` default. `[window] colorspace` describes how
        // input color bytes are *interpreted*, not the surface gamut.
        assert_eq!(result.window.colorspace, window::Colorspace::Srgb);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn test_platform_specific_env_vars() {
        let mut result = create_temporary_config(
            "platform-env-vars",
            r#"
            env-vars = ["GLOBAL=value", "FOO=bar"]

            [platform]
            macos.env-vars = ["MACOS_ONLY=yes", "PLATFORM_VAR=macos"]
        "#,
        );

        // Apply platform overrides
        result.overwrite_based_on_platform();

        // Should have both global and platform-specific env vars
        assert_eq!(result.env_vars.len(), 4);
        assert!(result.env_vars.contains(&String::from("GLOBAL=value")));
        assert!(result.env_vars.contains(&String::from("FOO=bar")));
        assert!(result.env_vars.contains(&String::from("MACOS_ONLY=yes")));
        assert!(
            result
                .env_vars
                .contains(&String::from("PLATFORM_VAR=macos"))
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn test_platform_specific_env_vars_linux() {
        let mut result = create_temporary_config(
            "platform-env-vars-linux",
            r#"
            env-vars = ["GLOBAL=value"]

            [platform]
            linux.env-vars = ["LINUX_ONLY=yes"]
        "#,
        );

        result.overwrite_based_on_platform();

        assert_eq!(result.env_vars.len(), 2);
        assert!(result.env_vars.contains(&String::from("GLOBAL=value")));
        assert!(result.env_vars.contains(&String::from("LINUX_ONLY=yes")));
    }

    #[test]
    #[cfg(windows)]
    fn test_platform_specific_env_vars_windows() {
        let mut result = create_temporary_config(
            "platform-env-vars-windows",
            r#"
            env-vars = ["GLOBAL=value"]

            [platform]
            windows.env-vars = ["WINDOWS_ONLY=yes"]
        "#,
        );

        result.overwrite_based_on_platform();

        assert_eq!(result.env_vars.len(), 2);
        assert!(result.env_vars.contains(&String::from("GLOBAL=value")));
        assert!(result.env_vars.contains(&String::from("WINDOWS_ONLY=yes")));
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn test_platform_window_field_level_merge() {
        let mut result = create_temporary_config(
            "platform-window-merge",
            r#"
            [window]
            width = 800
            height = 600
            opacity = 0.75
            blur = true

            [platform]
            macos.window.mode = "Maximized"
        "#,
        );

        result.overwrite_based_on_platform();

        // Mode should be overridden
        assert_eq!(result.window.mode, window::WindowMode::Maximized);
        // But other fields should be preserved
        assert_eq!(result.window.width, 800);
        assert_eq!(result.window.height, 600);
        assert_eq!(result.window.opacity, 0.75);
        assert!(result.window.blur.is_enabled());
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn test_platform_shell_replace() {
        let mut result = create_temporary_config(
            "platform-shell-replace",
            r#"
            shell = { program = "/bin/bash", args = ["--login"] }

            [platform]
            macos.shell = { program = "/bin/zsh", args = ["-l"] }
        "#,
        );

        result.overwrite_based_on_platform();

        // Shell should be completely replaced
        assert_eq!(result.shell.program, "/bin/zsh");
        assert_eq!(result.shell.args, vec!["-l"]);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn test_platform_renderer_merge() {
        let mut result = create_temporary_config(
            "platform-renderer-merge",
            r#"
            [renderer]
            performance = "High"
            disable-unfocused-render = true

            [platform]
            macos.renderer.backend = "Cpu"
        "#,
        );

        result.overwrite_based_on_platform();

        // Backend should be set
        assert_eq!(result.renderer.backend, crate::renderer::Backend::Cpu);
        // Other fields should be preserved
        assert!(result.renderer.disable_unfocused_render);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn test_platform_navigation_merge() {
        let mut result = create_temporary_config(
            "platform-navigation-merge",
            r#"
            [navigation]
            mode = "Tab"
            clickable = true

            [platform]
            macos.navigation.mode = "NativeTab"
        "#,
        );

        result.overwrite_based_on_platform();

        // Mode should be overridden
        assert_eq!(
            result.navigation.mode,
            navigation::NavigationMode::NativeTab
        );
        // Clickable should be preserved
        assert!(result.navigation.clickable);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn test_platform_theme_override() {
        let mut result = create_temporary_config(
            "platform-theme-override",
            r#"
            theme = "default-theme"

            [platform]
            macos.theme = "macos-specific-theme"
        "#,
        );

        result.overwrite_based_on_platform();

        // Theme should be overridden
        assert_eq!(result.theme, "macos-specific-theme");
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn test_platform_complex_merge() {
        let mut result = create_temporary_config(
            "platform-complex-merge",
            r#"
            env-vars = ["GLOBAL=1"]
            theme = "default"

            [window]
            width = 1024
            height = 768
            opacity = 0.9
            blur = false

            [renderer]
            performance = "Low"
            disable-unfocused-render = false

            [navigation]
            mode = "Tab"
            clickable = false

            shell = { program = "/bin/sh", args = ["-c"] }

            [platform]
            macos.env-vars = ["MACOS=1"]
            macos.theme = "macos-theme"
            macos.window.opacity = 1.0
            macos.window.blur = true
            macos.renderer.performance = "High"
            macos.navigation.clickable = true
            macos.shell = { program = "/bin/zsh", args = ["--login"] }
        "#,
        );

        result.overwrite_based_on_platform();

        // Env vars should be merged
        assert!(result.env_vars.contains(&String::from("GLOBAL=1")));
        assert!(result.env_vars.contains(&String::from("MACOS=1")));

        // Theme overridden
        assert_eq!(result.theme, "macos-theme");

        // Window: opacity and blur overridden, others preserved
        assert_eq!(result.window.opacity, 1.0);
        assert!(result.window.blur.is_enabled());
        assert_eq!(result.window.width, 1024);
        assert_eq!(result.window.height, 768);

        // Renderer: performance overridden, disable_unfocused_render preserved
        assert!(!result.renderer.disable_unfocused_render);

        // Navigation: clickable overridden, mode preserved
        assert!(result.navigation.clickable);
        assert_eq!(result.navigation.mode, navigation::NavigationMode::Tab);

        // Shell: completely replaced
        assert_eq!(result.shell.program, "/bin/zsh");
        assert_eq!(result.shell.args, vec!["--login"]);
    }

    #[test]
    fn test_multiple_platform_configs_dont_interfere() {
        let result = create_temporary_config(
            "multi-platform",
            r#"
            env-vars = ["GLOBAL=1"]

            [platform]
            linux.env-vars = ["LINUX=1"]
            windows.env-vars = ["WINDOWS=1"]
            macos.env-vars = ["MACOS=1"]
        "#,
        );

        // Before applying platform overrides, should only have global env vars
        assert_eq!(result.env_vars.len(), 1);
        assert!(result.env_vars.contains(&String::from("GLOBAL=1")));
    }
}
