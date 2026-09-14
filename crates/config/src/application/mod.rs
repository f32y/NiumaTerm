#[cfg(test)]
mod tests;

use std::default::Default;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::{env, fs, io, mem};

use dirs::home_dir;
use nmt_platform::environment::config_dir;
use serde::{Deserialize, Serialize};
use toml::from_str as parse_toml;
use toml_edit::{DocumentMut, Item, Table, value};
use tracing::warn;

use crate::agent::AgentConfig;
use crate::appearance::AppearanceConfig;
use crate::builtin_themes::{THEMES as BUILTIN_THEMES, get as get_builtin_theme};
use crate::colors::Colors;
use crate::defaults::*;
use crate::profile::Profile;
use crate::system::{self, SystemConfig};
use crate::terminal::TerminalConfig;
#[cfg(test)]
use crate::theme::AppearanceTheme;
use crate::theme::{Theme, UiTheme};
use crate::{
    CursorShape, agent, appearance, persistence, profile, remote_session, set_active_colors, update,
};

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

    #[serde(default)]
    pub terminal: TerminalConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CursorConfig {
    #[serde(default = "default_cursor")]
    pub shape: CursorShape,
}

static TESTING_MODE: AtomicBool = AtomicBool::new(false);

/// Select the isolated `Test` configuration directory before configuration is loaded.
pub fn enable_testing_mode() {
    TESTING_MODE.store(true, Ordering::Relaxed);
}

fn config_dir_for_mode(path: PathBuf, testing: bool) -> PathBuf {
    if testing { path.join("Test") } else { path }
}

/// Home directory with a temp-dir fallback: a session without a resolvable
/// home (stripped-down service accounts) gets per-boot config instead of a
/// startup panic.
fn home_dir_or_temp() -> PathBuf {
    home_dir().unwrap_or_else(env::temp_dir)
}

#[inline]
fn base_config_dir_path() -> PathBuf {
    env::var("NMT_CONFIG_HOME")
        .map(Into::into)
        .unwrap_or_else(|_| config_dir(&home_dir_or_temp()))
}

#[inline]
pub fn config_dir_path() -> PathBuf {
    config_dir_for_mode(base_config_dir_path(), TESTING_MODE.load(Ordering::Relaxed))
}

#[inline]
pub fn config_file_path() -> PathBuf {
    config_dir_path().join("config.toml")
}

fn theme_file_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.toml"))
}

impl Config {
    fn load_theme(path: &PathBuf) -> Result<Theme, String> {
        let content = if path.exists() {
            fs::read_to_string(path).map_err(|err| err.to_string())?
        } else {
            let name = path
                .file_stem()
                .and_then(|name| name.to_str())
                .ok_or_else(|| -> String { "invalid theme filepath".into() })?;

            get_builtin_theme(name)
                .map(str::to_owned)
                .ok_or_else(|| -> String { "filepath does not exist".into() })?
        };

        let mut theme = parse_toml::<Theme>(&content)
            .map_err(|err_message| format!("error parsing: {err_message:?}"))?;

        // Older installations may have copied built-in files before family
        // metadata existed. Keep those copies paired while preserving their
        // customized colors and unrelated user themes.
        if theme.family.is_empty()
            && let Some(name) = path.file_stem().and_then(|name| name.to_str())
            && let Some(source) = get_builtin_theme(name)
            && let Ok(builtin) = parse_toml::<Theme>(source)
            && builtin.name == theme.name
            && builtin.mode == theme.mode
        {
            theme.family = builtin.family;
        }

        Ok(theme)
    }

    /// Load a named theme from the per-user `themes` directory.
    pub fn load_named_theme(name: &str) -> Result<Theme, String> {
        let path = Path::new(name);

        if path.file_name().and_then(|name| name.to_str()) != Some(name) {
            return Err("theme name must not contain a path".into());
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

        themes.sort_by_cached_key(|(name, _)| name.to_lowercase());

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

        themes.sort_by_cached_key(|(name, _)| name.to_lowercase());

        themes
    }

    pub fn load_for_startup_from(path: &Path, config_dir: &Path) -> Result<Self, String> {
        let content = match fs::read_to_string(path) {
            Ok(content) => content,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Config::default()),
            Err(error) => return Err(format!("failed to read {}: {error}", path.display())),
        };

        let mut decoded = parse_toml::<Config>(&content).map_err(|error| error.to_string())?;

        decoded.appearance.normalize();

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
            terminal: TerminalConfig::default(),
        }
    }
}

impl Default for CursorConfig {
    fn default() -> Self {
        Self {
            shape: default_cursor(),
        }
    }
}

static CONFIG: OnceLock<Config> = OnceLock::new();

pub fn init(config: Config) {
    let colors = config.colors;

    assert!(
        CONFIG.set(config).is_ok(),
        "configuration already initialized"
    );

    set_active_colors(colors);
}

pub fn get() -> &'static Config {
    CONFIG.get_or_init(Config::default)
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
    pub terminal: &'a TerminalConfig,
}

/// Save settings to an explicit configuration path using the same locked,
/// atomic update as the default user configuration.
pub fn save_settings_to(path: &Path, patch: &SettingsPatch<'_>) -> io::Result<()> {
    persistence::update(path, |content| {
        let mut doc = match content {
            Some(content) => content.parse::<DocumentMut>().map_err(|err| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("config.toml is not valid TOML, not saving settings: {err}"),
                )
            })?,
            None => DocumentMut::new(),
        };

        // Credential encryption runs while patching, before any file is touched;
        // a failure here must leave the existing configuration file as it is.
        patch_settings_document(&mut doc, patch).map_err(|err| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("not saving settings: {err}"),
            )
        })?;

        Ok(doc.to_string())
    })
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
        terminal,
    } = patch;

    doc["theme"] = value(theme);

    patch_group(doc, "appearance", appearance)?;

    if appearance.background_image.is_none() {
        doc["appearance"]
            .as_table_mut()
            .expect("appearance is a table")
            .remove("background-image");
    }

    ensure_explicit_table(doc, "cursor");

    doc["cursor"]["shape"] = value::<&str>(cursor_shape.into());

    patch_group(doc, "system", system)?;

    patch_group(doc, "agent", agent)?;

    patch_group(doc, "remote-session", remote_session)?;

    patch_group(doc, "update", update)?;

    profile::patch_table(
        ensure_explicit_table(doc, "profiles"),
        profiles,
        default_profile,
    );

    profile::patch_agent_table(
        ensure_explicit_table(doc, "agent-profiles"),
        agent_profiles,
        default_agent_profile,
    )?;

    patch_group(doc, "terminal", terminal)
}

/// Each group's serde names also define the keys edited by the settings UI.
/// Updating individual entries retains keys this build does not recognize.
fn patch_group(doc: &mut DocumentMut, key: &str, settings: &impl Serialize) -> Result<(), String> {
    let serialized = toml::to_string(settings).map_err(|error| error.to_string())?;

    let values = serialized
        .parse::<DocumentMut>()
        .map_err(|error| error.to_string())?;

    ensure_explicit_table(doc, key);

    let table = doc[key].as_table_mut().expect("settings group is a table");

    for (name, item) in values.iter() {
        let target = table.entry(name).or_insert(Item::None);

        let mut item = item.clone();

        if let (Some(previous), Some(next)) = (target.as_value(), item.as_value_mut()) {
            *next.decor_mut() = previous.decor().clone();
        }

        *target = item;
    }

    Ok(())
}

/// Make `doc[key]` an explicit table so nested managed keys never turn into an
/// inline table and existing inline or malformed values are normalized safely.
fn ensure_explicit_table<'a>(doc: &'a mut DocumentMut, key: &str) -> &'a mut Table {
    let item = doc.entry(key).or_insert_with(|| Item::Table(Table::new()));

    if !item.is_table() {
        let previous = mem::replace(item, Item::None);

        *item = Item::Table(previous.into_table().unwrap_or_default());
    }

    item.as_table_mut().expect("entry is a table")
}
