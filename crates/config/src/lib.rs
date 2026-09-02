pub mod agent;
pub mod appearance;
pub mod builtin_themes;
pub mod colors;
mod credentials;
pub mod defaults;
pub mod local_state;
pub mod profile;
pub mod remote_session;
pub mod render_types;
pub mod system;
pub mod theme;
pub mod update;

use std::default::Default;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{OnceLock, RwLock};
use std::{env, fs, io, mem};

use dirs::home_dir;
#[cfg(target_os = "windows")]
use nmt_platform::windows::environment::config_dir as windows_config_dir;
use serde::{Deserialize, Serialize};
#[cfg(test)]
use theme::AppearanceTheme;
use theme::{Theme, UiTheme};
use toml::de::Error as TomlDeError;
use toml::from_str as parse_toml;
use toml_edit::{DocumentMut, Item, Table, value};
use tracing::warn;

use crate::agent::AgentConfig;
use crate::appearance::AppearanceConfig;
use crate::builtin_themes::{THEMES as BUILTIN_THEMES, get as get_builtin_theme};
use crate::colors::Colors;
use crate::defaults::*;
use crate::profile::Profile;
use crate::system::SystemConfig;

#[derive(Default, Debug, Serialize, Deserialize, PartialEq, Clone)]
pub struct Shell {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Config {
    #[serde(default)]
    pub cursor: CursorConfig,
    #[serde(default = "default_shell")]
    pub shell: Shell,
    #[serde(default = "default_working_dir", rename = "working-dir")]
    pub working_dir: Option<String>,
    #[serde(default = "default_theme")]
    pub theme: String,
    #[serde(default = "default_editor")]
    pub editor: Shell,
    #[serde(skip)]
    pub colors: Colors,
    /// UI theme loaded from the selected file in `themes/`.
    #[serde(skip)]
    pub ui_theme: Option<UiTheme>,
    /// Visual settings (settings dialog, Terminal/Appearance pages).
    #[serde(default = "appearance::AppearanceConfig::default")]
    pub appearance: appearance::AppearanceConfig,
    /// The `[profiles]` section: default-profile name + profile entries.
    #[serde(default)]
    pub profiles: profile::ProfilesConfig,
    /// The `[agent-profiles]` section: default agent-profile name + entries.
    #[serde(default, rename = "agent-profiles")]
    pub agent_profiles: profile::AgentProfilesConfig,
    /// Agent integration settings (settings dialog, Agent page).
    #[serde(default = "agent::AgentConfig::default")]
    pub agent: agent::AgentConfig,
    /// System-behavior settings (settings dialog, System page).
    #[serde(default = "system::SystemConfig::default")]
    pub system: system::SystemConfig,
    /// Remote-session connection settings (settings dialog, Remote Session page).
    #[serde(default, rename = "remote-session")]
    pub remote_session: remote_session::RemoteSessionConfig,
    /// Update checking settings (settings dialog, About page).
    #[serde(default = "update::UpdateConfig::default")]
    pub update: update::UpdateConfig,
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

/// Home directory with a temp-dir fallback: a session without a resolvable
/// home (stripped-down service accounts) gets per-boot config instead of a
/// startup panic.
fn home_dir_or_temp() -> PathBuf {
    home_dir().unwrap_or_else(env::temp_dir)
}

#[cfg(target_os = "macos")]
#[inline]
fn base_config_dir_path() -> PathBuf {
    env::var("NMT_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or(home_dir_or_temp().join(".config").join("NiumaTerm"))
}

#[cfg(target_os = "windows")]
#[inline]
fn base_config_dir_path() -> PathBuf {
    env::var("NMT_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| windows_config_dir(&home_dir_or_temp()))
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
#[inline]
fn base_config_dir_path() -> PathBuf {
    env::var("NMT_CONFIG_HOME").map(PathBuf::from).unwrap_or(
        env::var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or(home_dir_or_temp().join(".config"))
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
            let content = fs::read_to_string(path).unwrap();
            let decoded: Config = parse_toml(&content).unwrap_or_else(|_| Config::default());
            decoded
        } else {
            Config::default()
        }
    }
    #[cfg(test)]
    fn load_from_path_without_fallback(path: &PathBuf) -> Result<Self, String> {
        if path.exists() {
            let content = fs::read_to_string(path).unwrap();
            match parse_toml::<Config>(&content) {
                Ok(mut decoded) => {
                    let theme = &decoded.theme;
                    if theme.is_empty() {
                        return Ok(decoded);
                    }

                    let tmp = env::temp_dir();
                    let path = theme_file_path(&tmp, theme);
                    if let Ok(loaded_theme) = Config::load_theme(&path) {
                        decoded.ui_theme = loaded_theme.ui_theme();
                        decoded.colors = loaded_theme.colors.terminal;
                    } else {
                        warn!("failed to load theme: {}", theme);
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
            fs::read_to_string(path).map_err(|err| err.to_string())?
        } else {
            let name = path
                .file_stem()
                .and_then(|name| name.to_str())
                .ok_or_else(|| String::from("invalid theme filepath"))?;
            get_builtin_theme(name)
                .map(str::to_owned)
                .ok_or_else(|| String::from("filepath does not exist"))?
        };
        parse_toml::<Theme>(&content)
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
        let mut themes = BUILTIN_THEMES
            .iter()
            .filter_map(|builtin| match parse_toml::<Theme>(builtin.source) {
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
        let Ok(entries) = fs::read_dir(path) else {
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

    pub fn load_for_startup() -> Result<Self, TomlDeError> {
        Config::load_for_startup_from(&config_file_path(), &config_dir_path())
    }

    fn load_for_startup_from(path: &Path, config_dir: &Path) -> Result<Self, TomlDeError> {
        let Some(content) = fs::read_to_string(path).ok() else {
            return Ok(Config::default());
        };
        let mut decoded = parse_toml::<Config>(&content)?;
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
            colors: Colors::default(),
            ui_theme: None,
            shell: default_shell(),
            theme: default_theme(),
            working_dir: default_working_dir(),
            appearance: appearance::AppearanceConfig::default(),
            profiles: profile::ProfilesConfig::default(),
            agent_profiles: profile::AgentProfilesConfig::default(),
            agent: agent::AgentConfig::default(),
            system: system::SystemConfig::default(),
            remote_session: remote_session::RemoteSessionConfig::default(),
            update: update::UpdateConfig::default(),
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
    #[serde(alias = "beam", alias = "line", alias = "Line")]
    Beam,
    /// Cursor is hidden.
    #[serde(alias = "hidden")]
    Hidden,
}

impl CursorShape {
    pub fn as_str(self) -> &'static str {
        match self {
            CursorShape::Block => "block",
            CursorShape::Underline => "underline",
            CursorShape::Beam => "line",
            CursorShape::Hidden => "hidden",
        }
    }

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

pub fn init(config: Config) {
    set_active_colors(config.colors);
    let _ = CONFIG.set(config);
}

pub fn get() -> &'static Config {
    CONFIG.get_or_init(Config::default)
}

/// Read from the active terminal palette under its lock. `Colors` carries a
/// field per palette entry, so a caller after one of them reads it here rather
/// than copying several hundred bytes out to reach it.
pub fn with_active_colors<T>(read: impl FnOnce(&Colors) -> T) -> T {
    read(
        &ACTIVE_COLORS
            .get_or_init(|| RwLock::new(get().colors))
            .read()
            .expect("active theme colors lock poisoned"),
    )
}

/// Return the active terminal palette. Unlike the rest of the startup config,
/// this value can change when the user selects a theme.
pub fn active_colors() -> Colors {
    with_active_colors(|colors| *colors)
}

pub fn set_active_colors(colors: Colors) {
    *ACTIVE_COLORS
        .get_or_init(|| RwLock::new(colors))
        .write()
        .expect("active theme colors lock poisoned") = colors;
}

/// The settings-dialog values written back to config.toml in one patch.
pub struct SettingsPatch<'a> {
    pub theme: &'a str,
    pub appearance: &'a AppearanceConfig,
    pub cursor_shape: CursorShape,
    pub agent: &'a AgentConfig,
    pub system: &'a SystemConfig,
    pub remote_session: &'a remote_session::RemoteSessionConfig,
    pub update: &'a update::UpdateConfig,
    pub profiles: &'a [Profile],
    pub default_profile: &'a str,
    pub agent_profiles: &'a [profile::AgentProfile],
    pub default_agent_profile: &'a str,
}

pub fn save_settings(patch: &SettingsPatch<'_>) -> io::Result<()> {
    save_settings_to(&config_file_path(), patch)
}

fn save_settings_to(path: &Path, patch: &SettingsPatch<'_>) -> io::Result<()> {
    let mut doc = match fs::read_to_string(path) {
        Ok(content) => content.parse::<DocumentMut>().map_err(|err| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("config.toml is not valid TOML, not saving settings: {err}"),
            )
        })?,
        // Missing (or unreadable) file: start from an empty document.
        Err(_) => DocumentMut::new(),
    };

    // Credential encryption runs while patching, before any file is touched;
    // a failure here must leave the existing configuration file as it is.
    patch_settings_document(&mut doc, patch).map_err(|err| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("not saving settings: {err}"),
        )
    })?;

    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("toml.tmp");
    fs::write(&tmp, doc.to_string())?;
    fs::rename(&tmp, path)?;
    Ok(())
}

fn patch_settings_document(doc: &mut DocumentMut, patch: &SettingsPatch<'_>) -> Result<(), String> {
    let &SettingsPatch {
        theme,
        appearance,
        cursor_shape,
        agent,
        system,
        profiles,
        remote_session,
        update,
        default_profile,
        agent_profiles,
        default_agent_profile,
    } = patch;

    doc["theme"] = value(theme);
    ensure_explicit_table(doc, "appearance");
    doc["appearance"]["input-style"] = value(appearance.input_style.as_str());
    doc["appearance"]["scroll-to-bottom-when-typing"] =
        value(appearance.scroll_to_bottom_when_typing);
    doc["appearance"]["agent-pane-use-terminal-background"] =
        value(appearance.agent_pane_use_terminal_background);
    doc["appearance"]["command-blocks"] = value(appearance.command_blocks);
    doc["appearance"]["show-daily-token-usage"] = value(appearance.show_daily_token_usage);
    doc["appearance"]["show-git-status-on-title-bar"] =
        value(appearance.show_git_status_on_title_bar);
    doc["appearance"]["git-status-refresh-interval"] =
        value(appearance.git_status_refresh_interval as i64);
    doc["appearance"]["tab-width"] = value(appearance.tab_width);
    doc["appearance"]["tab-auto-size"] = value(appearance.tab_auto_size);
    doc["appearance"]["tab-bar-style"] = value(appearance.tab_bar_style.as_str());
    doc["appearance"]["ui-font"] = value(&appearance.ui_font);
    doc["appearance"]["terminal-font-family"] = value(&appearance.terminal_font_family);
    doc["appearance"]["terminal-font-size"] = value(appearance.terminal_font_size);
    doc["appearance"]["terminal-line-height"] = value(appearance.terminal_line_height);
    doc["appearance"]["agent-font-family"] = value(&appearance.agent_font_family);
    doc["appearance"]["agent-font-size"] = value(appearance.agent_font_size);
    doc["appearance"]["monospace-only"] = value(appearance.monospace_only);
    doc["appearance"]["enable-window-transparency"] = value(appearance.window_backdrop.as_str());
    doc["appearance"]["transparent-main-view"] = value(appearance.transparent_main_view);
    doc["appearance"]["smooth-scrolling"] = value(appearance.smooth_scrolling.as_str());
    doc["appearance"]["background-opacity"] = value(appearance.background_opacity);
    if let Some(path) = &appearance.background_image {
        doc["appearance"]["background-image"] = value(path);
    } else {
        doc["appearance"]
            .as_table_mut()
            .expect("appearance was normalized to a table")
            .remove("background-image");
    }
    doc["appearance"]["background-image-opacity"] = value(appearance.background_image_opacity);
    doc["appearance"]["language"] = value(appearance.language.as_str());
    doc["appearance"]["agent-transcript-font-family"] =
        value(&appearance.agent_transcript_font_family);
    doc["appearance"]["agent-transcript-font-size"] = value(appearance.agent_transcript_font_size);
    doc["appearance"]["reduce-motion"] = value(appearance.reduce_motion);

    ensure_explicit_table(doc, "cursor");
    doc["cursor"]["shape"] = value(cursor_shape.as_str());

    ensure_explicit_table(doc, "system");
    system::patch_document(doc, system);

    ensure_explicit_table(doc, "agent");
    agent::patch_document(doc, agent);

    ensure_explicit_table(doc, "remote-session");
    remote_session::patch_document(doc, remote_session);

    ensure_explicit_table(doc, "update");
    update::patch_document(doc, update);

    profile::patch_document(doc, profiles, default_profile);
    profile::patch_agent_document(doc, agent_profiles, default_agent_profile)
}

/// Make `doc[key]` an explicit table so nested managed keys never turn into an
/// inline table and existing inline or malformed values are normalized safely.
pub(crate) fn ensure_explicit_table(doc: &mut DocumentMut, key: &str) {
    let item = doc.entry(key).or_insert_with(|| Item::Table(Table::new()));
    if !item.is_table() {
        let previous = mem::replace(item, Item::None);
        *item = Item::Table(previous.into_table().unwrap_or_default());
    }
}

#[cfg(test)]
mod tests;
