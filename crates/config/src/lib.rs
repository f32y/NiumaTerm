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

use std::default::Default;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{OnceLock, RwLock};
use std::{env, fs, io, mem};

use builtin_themes::{THEMES as BUILTIN_THEMES, get as get_builtin_theme};
use colors::Colors;
use dirs::home_dir;
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
    env::var("NMT_CONFIG_HOME").map(PathBuf::from).unwrap_or(
        home_dir_or_temp()
            .join("AppData")
            .join("Local")
            .join("NiumaTerm"),
    )
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

/// The settings-dialog values written back to config.toml in one patch.
pub struct SettingsPatch<'a> {
    pub theme: &'a str,
    pub appearance: &'a AppearanceConfig,
    pub cursor_shape: CursorShape,
    pub agent: &'a AgentConfig,
    pub system: &'a SystemConfig,
    pub remote_session: &'a remote_session::RemoteSessionConfig,
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

    ensure_explicit_table(doc, "cursor");
    doc["cursor"]["shape"] = value(cursor_shape.as_str());

    ensure_explicit_table(doc, "system");
    system::patch_document(doc, system);

    ensure_explicit_table(doc, "agent");
    agent::patch_document(doc, agent);

    ensure_explicit_table(doc, "remote-session");
    remote_session::patch_document(doc, remote_session);

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
mod tests {
    use std::io::Write;

    use colors::hex_to_color_arr;

    use super::*;

    fn sample_appearance() -> AppearanceConfig {
        AppearanceConfig {
            input_style: appearance::InputStyle::Waterfall,
            scroll_to_bottom_when_typing: false,
            agent_pane_use_terminal_background: true,
            command_blocks: false,
            show_daily_token_usage: true,
            show_git_status_on_title_bar: true,
            git_status_refresh_interval: 15,
            tab_width: 150.0,
            tab_auto_size: true,
            tab_bar_style: appearance::TabBarStyle::Vertical,
            ui_font: "Arial".to_string(),
            terminal_font_family: "Cascadia Code".to_string(),
            terminal_font_size: 16.0,
            terminal_line_height: 1.2,
            agent_font_family: "Cascadia Code".to_string(),
            agent_font_size: 15.0,
            monospace_only: false,
            window_backdrop: appearance::WindowBackdrop::Acrylic,
            transparent_main_view: false,
            smooth_scrolling: appearance::SmoothScrollingMode::Off,
            background_opacity: 0.85,
            background_image: Some(r"C:\Wallpapers\background.png".to_string()),
            background_image_opacity: 0.4,
            language: appearance::Language::ZhCn,
            agent_transcript_font_family: "JetBrains Mono".to_string(),
            agent_transcript_font_size: 12.5,
        }
    }

    fn sample_system() -> SystemConfig {
        SystemConfig {
            restore_last_session_when_opening: false,
            manage_subprocess_job: true,
            warn_before_terminating_shell: system::WarnBeforeTerminatingShell::Disabled,
            confirm_before_closing_workspace: false,
            prioritize_ui_threads: true,
            newline_shortcut: system::NewlineShortcut::ShiftEnter,
            open_in_best_workspace: false,
        }
    }

    fn sample_agent() -> AgentConfig {
        AgentConfig {
            enable_agent_hooks: false,
            show_agent_usage: false,
            collapse_tool_calls: agent::CollapseRows::WorkAndToolCalls,
            check_agent_updates: false,
            codex_skill_command_compat: false,
        }
    }

    fn sample_profiles() -> Vec<Profile> {
        vec![Profile {
            name: "PowerShell".to_string(),
            shell: r"C:\WINDOWS\System32\WindowsPowerShell\v1.0\powershell.exe".to_string(),
            args: "-NoLogo".to_string(),
        }]
    }

    fn sample_agent_profiles() -> Vec<profile::AgentProfile> {
        vec![profile::AgentProfile {
            name: "Claude Code".to_string(),
            kind: profile::AgentProfileKind::ClaudeCode,
            executable: "claude".to_string(),
            via_npx: false,
            model: "claude-opus-4-8".to_string(),
            effort: "high".to_string(),
            replace_sub_models: true,
            use_custom_endpoint: true,
            cache_warn_minutes: 30,
            api_base_url: "https://proxy.example.com".to_string(),
            api_key: "sk-test".to_string(),
            env: vec![profile::EnvVar {
                name: "FOO".to_string(),
                value: "bar".to_string(),
            }],
        }]
    }

    fn patch_settings(doc: &mut DocumentMut) {
        patch_settings_document(
            doc,
            &SettingsPatch {
                theme: "test-theme",
                appearance: &sample_appearance(),
                cursor_shape: CursorShape::Beam,
                agent: &sample_agent(),
                system: &sample_system(),
                remote_session: &remote_session::RemoteSessionConfig::default(),
                profiles: &sample_profiles(),
                default_profile: "PowerShell",
                agent_profiles: &sample_agent_profiles(),
                default_agent_profile: "Claude Code",
            },
        )
        .unwrap();
    }

    #[test]
    fn settings_patch_preserves_comments_and_unrelated_keys() {
        let existing = "# my terminal config\ntheme = \"dark\"\n\n[window]\nwidth = 960\n";
        let mut doc = existing.parse::<DocumentMut>().unwrap();

        patch_settings(&mut doc);
        let out = doc.to_string();

        assert!(out.contains("# my terminal config"));
        assert!(out.contains("width = 960"));
        assert!(out.contains("smooth-scrolling = \"off\""));
        assert!(out.contains("agent-transcript-font-family = \"JetBrains Mono\""));
        assert!(out.contains("agent-transcript-font-size = 12.5"));

        let config: Config = parse_toml(&out).unwrap();
        assert_eq!(config.appearance, sample_appearance());
        assert_eq!(config.agent, sample_agent());
        assert_eq!(config.system, sample_system());
        assert_eq!(config.profiles.list, sample_profiles());
        assert_eq!(config.profiles.default, "PowerShell");
        assert_eq!(config.agent_profiles.list, sample_agent_profiles());
        assert_eq!(config.agent_profiles.default, "Claude Code");
        assert!(config.agent_profiles.initialized);
        assert_eq!(config.cursor.shape, CursorShape::Beam);
    }

    #[test]
    fn settings_patch_converts_inline_tables() {
        let mut doc =
            "fonts = { size = 12.0, hinting = true }\nappearance = { monospace-only = false }\n"
                .parse::<DocumentMut>()
                .unwrap();
        patch_settings(&mut doc);

        let out = doc.to_string();
        assert!(out.contains("fonts = { size = 12.0, hinting = true }"));
        let config: Config = parse_toml(&out).unwrap();
        assert_eq!(config.appearance, sample_appearance());
    }

    #[test]
    fn save_settings_to_creates_updates_and_rejects_invalid() {
        let dir = env::temp_dir().join("NiumaTerm-settings-save-test");
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("config.toml");

        let save = || {
            save_settings_to(
                &path,
                &SettingsPatch {
                    theme: "test-theme",
                    appearance: &sample_appearance(),
                    cursor_shape: CursorShape::Beam,
                    agent: &sample_agent(),
                    system: &sample_system(),
                    remote_session: &remote_session::RemoteSessionConfig::default(),
                    profiles: &sample_profiles(),
                    default_profile: "PowerShell",
                    agent_profiles: &sample_agent_profiles(),
                    default_agent_profile: "Claude Code",
                },
            )
        };

        save().unwrap();
        let config: Config = parse_toml(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(config.appearance, sample_appearance());
        assert_eq!(config.agent, sample_agent());
        assert_eq!(config.theme, "test-theme");

        save().unwrap();
        let config: Config = parse_toml(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(config.profiles.default, "PowerShell");
        assert!(!path.with_extension("toml.tmp").exists());

        fs::write(&path, "not [ valid").unwrap();
        assert!(save().is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "not [ valid");

        let _ = fs::remove_dir_all(&dir);
    }

    /// The stored `api-credentials` string of the first agent profile.
    fn stored_credentials(doc: &DocumentMut) -> String {
        doc["agent-profiles"]["list"]
            .as_array_of_tables()
            .unwrap()
            .get(0)
            .unwrap()["api-credentials"]
            .as_str()
            .unwrap()
            .to_string()
    }

    #[test]
    fn saved_agent_credentials_contain_no_plaintext() {
        let mut doc = DocumentMut::new();
        patch_settings(&mut doc);
        let out = doc.to_string();

        assert!(out.contains("api-credentials = \"aes256gcm-v1:"));
        assert!(!out.contains("proxy.example.com"));
        assert!(!out.contains("sk-test"));
        assert!(!out.contains("api-base-url"));
        assert!(!out.contains("api-key"));

        let config: Config = parse_toml(&out).unwrap();
        assert_eq!(config.agent_profiles.list, sample_agent_profiles());
    }

    #[test]
    fn repeated_saves_produce_different_stored_credentials() {
        let mut first = DocumentMut::new();
        patch_settings(&mut first);
        let mut second = DocumentMut::new();
        patch_settings(&mut second);

        assert_ne!(stored_credentials(&first), stored_credentials(&second));

        let restored: Config = parse_toml(&second.to_string()).unwrap();
        assert_eq!(restored.agent_profiles.list, sample_agent_profiles());
    }

    #[test]
    fn empty_agent_credentials_are_omitted() {
        let profiles = vec![profile::AgentProfile {
            name: "Plain".to_string(),
            ..profile::AgentProfile::default()
        }];
        let mut doc = DocumentMut::new();
        profile::patch_agent_document(&mut doc, &profiles, "Plain").unwrap();
        let out = doc.to_string();

        assert!(!out.contains("api-credentials"));
        assert!(!out.contains("api-base-url"));
        assert!(!out.contains("api-key"));
    }

    const LEGACY_PROFILE_TOML: &str = r#"
[[agent-profiles.list]]
name = "Legacy"
kind = "claude-code"
executable = "claude"
use-custom-endpoint = true
api-base-url = "https://legacy.example.com"
api-key = "sk-legacy"
"#;

    #[test]
    fn legacy_plaintext_credentials_load_without_touching_the_file() {
        let dir = tmp_dir().join("NiumaTerm-legacy-credentials-test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        fs::write(&path, LEGACY_PROFILE_TOML).unwrap();

        let config = Config::load_for_startup_from(&path, &dir).unwrap();
        let profile = &config.agent_profiles.list[0];
        assert_eq!(profile.api_base_url, "https://legacy.example.com");
        assert_eq!(profile.api_key, "sk-legacy");
        assert_eq!(fs::read_to_string(&path).unwrap(), LEGACY_PROFILE_TOML);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn legacy_plaintext_credentials_migrate_on_save() {
        let config: Config = parse_toml(LEGACY_PROFILE_TOML).unwrap();
        let mut doc = LEGACY_PROFILE_TOML.parse::<DocumentMut>().unwrap();
        profile::patch_agent_document(&mut doc, &config.agent_profiles.list, "Legacy").unwrap();
        let out = doc.to_string();

        assert!(out.contains("api-credentials = \"aes256gcm-v1:"));
        assert!(!out.contains("api-base-url"));
        assert!(!out.contains("api-key"));
        assert!(!out.contains("sk-legacy"));

        let restored: Config = parse_toml(&out).unwrap();
        let profile = &restored.agent_profiles.list[0];
        assert_eq!(profile.api_base_url, "https://legacy.example.com");
        assert_eq!(profile.api_key, "sk-legacy");
    }

    #[test]
    fn encrypted_credentials_win_over_adjacent_legacy_fields() {
        let stored = credentials::encrypt("https://current.example.com", "sk-current").unwrap();
        let toml_str = format!(
            "[[agent-profiles.list]]\nname = \"Both\"\napi-credentials = \"{stored}\"\n\
             api-base-url = \"https://stale.example.com\"\napi-key = \"sk-stale\"\n"
        );

        let config: Config = parse_toml(&toml_str).unwrap();
        let profile = &config.agent_profiles.list[0];
        assert_eq!(profile.api_base_url, "https://current.example.com");
        assert_eq!(profile.api_key, "sk-current");
    }

    #[test]
    fn invalid_encrypted_credentials_fail_without_legacy_fallback() {
        let valid = credentials::encrypt("https://real.example.com", "sk-real").unwrap();
        // Corrupt the last Base64 character while keeping the text decodable.
        let mut modified = valid.clone();
        let last = modified.pop().unwrap();
        modified.push(if last == 'A' { 'B' } else { 'A' });

        for bad in [
            "aes256gcm-v1:@@not-base64@@".to_string(),
            "aes256gcm-v9:AAAA".to_string(),
            modified,
        ] {
            let toml_str = format!(
                "[[agent-profiles.list]]\nname = \"Broken\"\napi-credentials = \"{bad}\"\n\
                 api-base-url = \"https://stale.example.com\"\napi-key = \"sk-stale\"\n"
            );
            let err = parse_toml::<Config>(&toml_str).unwrap_err().to_string();
            assert!(err.contains("Broken"), "{err}");
            assert!(!err.contains("sk-real"), "{err}");
            assert!(!err.contains("sk-stale"), "{err}");
            let payload = bad.strip_prefix("aes256gcm-").unwrap_or(&bad);
            assert!(!err.contains(payload), "{err}");
        }
    }

    #[test]
    fn testing_mode_uses_test_subdirectory() {
        let base = PathBuf::from("NiumaTerm");
        assert_eq!(config_dir_for_mode(base.clone(), false), base);
        assert_eq!(config_dir_for_mode(base.clone(), true), base.join("Test"));
    }

    fn tmp_dir() -> PathBuf {
        env::temp_dir()
    }

    fn create_temporary_config(prefix: &str, toml_str: &str) -> Config {
        let file_name = tmp_dir().join(format!("test-rio-{prefix}-config.toml"));
        let mut file = fs::File::create(&file_name).unwrap();
        writeln!(file, "{toml_str}").unwrap();

        match Config::load_from_path_without_fallback(&file_name) {
            Ok(config) => config,
            Err(e) => panic!("{e}"),
        }
    }

    /// Terminal palette of the built-in default theme, which a config that
    /// doesn't name a theme resolves to.
    fn default_theme_colors() -> Colors {
        parse_toml::<Theme>(get_builtin_theme(&default_theme()).unwrap())
            .unwrap()
            .colors
            .terminal
    }

    fn create_temporary_theme(theme: &str, toml_str: &str) {
        let file_name = tmp_dir().join(theme).with_extension("toml");
        let mut file = fs::File::create(file_name).unwrap();
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
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("config.toml");

        let missing = Config::load_for_startup_from(&path, &dir).unwrap();
        assert_eq!(missing, Config::default());

        fs::create_dir_all(&dir).unwrap();
        fs::write(&path, "not [ valid").unwrap();
        assert!(Config::load_for_startup_from(&path, &dir).is_err());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_if_explicit_defaults_match() {
        // An empty config file must resolve to the explicit defaults.
        let result = create_temporary_config("defaults", "");

        assert_eq!(result.cursor.shape, default_cursor());
        assert_eq!(result.theme, default_theme());
        assert_eq!(result.cursor.shape, default_cursor());
        assert_eq!(result.shell, default_shell());

        // Colors
        assert_eq!(result.colors, default_theme_colors());
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
        let mut file = fs::File::create(&file_name).unwrap();
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
        assert_eq!(result.colors, default_theme_colors());

        let result = create_temporary_config(
            "change-cursor-line",
            r#"
            [cursor]
            shape = 'line'
        "#,
        );
        assert_eq!(result.cursor.shape, CursorShape::Beam);
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
        let dir = env::temp_dir().join("NiumaTerm-theme-list-test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("Zulu.toml"),
            "[colors.terminal]\nbackground = '#111111'\n",
        )
        .unwrap();
        fs::write(
            dir.join("alpha.toml"),
            "[colors.terminal]\nbackground = '#222222'\n",
        )
        .unwrap();
        fs::write(dir.join("invalid.toml"), "[colors\n").unwrap();
        fs::write(dir.join("ignored.txt"), "[colors.terminal]\n").unwrap();

        let themes = Config::load_themes_from(&dir);
        assert_eq!(
            themes
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            ["alpha", "Zulu"]
        );

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn built_in_themes_load_without_user_files() {
        for builtin in BUILTIN_THEMES {
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

    const EXAMPLE_CONFIG_PATH: &str = "../../assets/config-example.toml";

    /// `assets/config-example.toml` documents every key with its built-in
    /// default. Nothing regenerates it, so this compares it against the real
    /// serialized default: a key added, removed, or renamed on `Config` fails
    /// here instead of leaving the example advertising settings that no longer
    /// exist. Run with `--nocapture` to print the replacement content.
    #[test]
    fn example_config_matches_the_serialized_defaults() {
        let generated = toml::to_string_pretty(&Config::default()).expect("defaults serialize");
        let shipped =
            fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(EXAMPLE_CONFIG_PATH))
                .expect("example config is readable");
        let body = shipped
            .split_once("\n\n")
            .map(|(_, body)| body)
            .unwrap_or(shipped.as_str());
        if body.trim() != generated.trim() {
            println!("---- regenerated assets/config-example.toml body ----");
            println!("{generated}");
        }
        assert_eq!(
            body.trim(),
            generated.trim(),
            "assets/config-example.toml is out of date"
        );
    }
}
