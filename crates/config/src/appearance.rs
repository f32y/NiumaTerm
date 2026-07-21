//! Visual settings, persisted as the `[appearance]` section of `config.toml`
//! by the settings dialog. This module also hosts `save_settings`, the
//! dialog's single write path for every section it manages.
//!
//! Saving patches the existing file with `toml_edit`, so user comments,
//! key order, and formatting outside the managed keys are preserved.

use serde::{Deserialize, Serialize};
use toml_edit::{DocumentMut, Item, Table, value};

use crate::agent::AgentConfig;
use crate::profile::Profile;
use crate::remote_session::RemoteSession;
use crate::system::SystemConfig;

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

/// Persist the dialog-managed settings into `config.toml`: the `[appearance]`,
/// `[agent]`, `[system]`, `[remote-session]`, and `[[profiles]]` sections. All other file content is
/// preserved.
///
/// The write is atomic (temp file + rename). If an existing config file does
/// not parse as TOML it is left untouched, so a hand-editable file is never
/// clobbered; the error is returned to the caller.
pub fn save_settings(
    theme: &str,
    appearance: &AppearanceConfig,
    agent: &AgentConfig,
    system: &SystemConfig,
    remote_session: &RemoteSession,
    profiles: &[Profile],
    default_profile: &str,
) -> std::io::Result<()> {
    save_settings_to(
        &crate::config_file_path(),
        theme,
        appearance,
        agent,
        system,
        remote_session,
        profiles,
        default_profile,
    )
}

fn save_settings_to(
    path: &std::path::Path,
    theme: &str,
    appearance: &AppearanceConfig,
    agent: &AgentConfig,
    system: &SystemConfig,
    remote_session: &RemoteSession,
    profiles: &[Profile],
    default_profile: &str,
) -> std::io::Result<()> {
    let mut doc = match std::fs::read_to_string(path) {
        Ok(content) => content.parse::<DocumentMut>().map_err(|err| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("config.toml is not valid TOML, not saving settings: {err}"),
            )
        })?,
        // Missing (or unreadable) file: start from an empty document.
        Err(_) => DocumentMut::new(),
    };

    patch_document(
        &mut doc,
        theme,
        appearance,
        agent,
        system,
        remote_session,
        profiles,
        default_profile,
    );

    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, doc.to_string())?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Apply the managed keys to a parsed document.
#[allow(clippy::too_many_arguments)]
fn patch_document(
    doc: &mut DocumentMut,
    theme: &str,
    appearance: &AppearanceConfig,
    agent: &AgentConfig,
    system: &SystemConfig,
    remote_session: &RemoteSession,
    profiles: &[Profile],
    default_profile: &str,
) {
    doc["theme"] = value(theme);
    ensure_explicit_table(doc, "appearance");
    doc["appearance"]["input-style"] = value(appearance.input_style.as_str());
    doc["appearance"]["command-blocks"] = value(appearance.command_blocks);
    doc["appearance"]["show-daily-token-usage"] = value(appearance.show_daily_token_usage);
    doc["appearance"]["show-git-status-on-title-bar"] =
        value(appearance.show_git_status_on_title_bar);
    doc["appearance"]["git-status-refresh-interval"] =
        value(appearance.git_status_refresh_interval as i64);
    doc["appearance"]["tab-width"] = value(appearance.tab_width);
    doc["appearance"]["ui-font"] = value(&appearance.ui_font);
    doc["appearance"]["terminal-font-family"] = value(&appearance.terminal_font_family);
    doc["appearance"]["terminal-font-size"] = value(appearance.terminal_font_size);
    doc["appearance"]["terminal-line-height"] = value(appearance.terminal_line_height);
    doc["appearance"]["monospace-only"] = value(appearance.monospace_only);
    doc["appearance"]["enable-window-transparency"] = value(appearance.window_transparency_enabled);
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

    ensure_explicit_table(doc, "system");
    crate::system::patch_document(doc, system);

    ensure_explicit_table(doc, "remote-session");
    crate::remote_session::patch_document(doc, remote_session);

    ensure_explicit_table(doc, "agent");
    crate::agent::patch_document(doc, agent);

    crate::profile::patch_document(doc, profiles, default_profile);
}

/// Make `doc[key]` an explicit `[key]` table. Chained indexing alone would
/// create an inline `key = {...}`, which rejects nested array-of-tables; an
/// existing inline table is converted (keeping its keys), and a scalar of
/// the wrong type is replaced.
pub(crate) fn ensure_explicit_table(doc: &mut DocumentMut, key: &str) {
    let item = doc.entry(key).or_insert_with(|| Item::Table(Table::new()));
    if !item.is_table() {
        let previous = std::mem::replace(item, Item::None);
        *item = Item::Table(previous.into_table().unwrap_or_default());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_appearance() -> AppearanceConfig {
        AppearanceConfig {
            input_style: InputStyle::Waterfall,
            command_blocks: false,
            show_daily_token_usage: true,
            show_git_status_on_title_bar: true,
            git_status_refresh_interval: 15,
            tab_width: 150.0,
            ui_font: "Arial".to_string(),
            terminal_font_family: "Cascadia Code".to_string(),
            terminal_font_size: 16.0,
            terminal_line_height: 1.2,
            monospace_only: false,
            window_transparency_enabled: true,
            background_opacity: 0.85,
            background_image: Some(r"C:\Wallpapers\background.png".to_string()),
            background_image_opacity: 0.4,
        }
    }

    fn sample_system() -> SystemConfig {
        SystemConfig {
            restore_last_session_when_opening: false,
            manage_subprocess_job: true,
            warn_before_terminating_shell: crate::system::WarnBeforeTerminatingShell::Disabled,
            confirm_before_closing_workspace: false,
            prioritize_ui_threads: true,
        }
    }

    fn sample_remote_session() -> RemoteSession {
        RemoteSession { enabled: true }
    }

    fn sample_agent() -> AgentConfig {
        AgentConfig {
            enable_agent_hooks: false,
            show_agent_usage: false,
        }
    }

    fn sample_profiles() -> Vec<Profile> {
        vec![Profile {
            name: "PowerShell".to_string(),
            shell: r"C:\WINDOWS\System32\WindowsPowerShell\v1.0\powershell.exe".to_string(),
            args: "-NoLogo".to_string(),
        }]
    }

    fn patch(doc: &mut DocumentMut) {
        patch_document(
            doc,
            "test-theme",
            &sample_appearance(),
            &sample_agent(),
            &sample_system(),
            &sample_remote_session(),
            &sample_profiles(),
            "PowerShell",
        );
    }

    #[test]
    fn patch_preserves_comments_and_unrelated_keys() {
        let existing = "# my terminal config\ntheme = \"dark\"\n\n[window]\nwidth = 960\n";
        let mut doc = existing.parse::<DocumentMut>().unwrap();

        patch(&mut doc);
        let out = doc.to_string();

        assert!(out.contains("# my terminal config"));
        assert!(out.contains("width = 960"));

        // The output re-parses and the managed keys round-trip.
        let config: crate::Config = toml::from_str(&out).unwrap();
        assert_eq!(config.appearance, sample_appearance());
        assert_eq!(config.agent, sample_agent());
        assert_eq!(config.system, sample_system());
        assert_eq!(config.remote_session, sample_remote_session());
        assert_eq!(config.profiles.list, sample_profiles());
        assert_eq!(config.profiles.default, "PowerShell");
    }

    #[test]
    fn patch_converts_inline_tables() {
        // Hand-written inline tables must not lose their unmanaged keys.
        let mut doc =
            "fonts = { size = 12.0, hinting = true }\nappearance = { monospace-only = false }\n"
                .parse::<DocumentMut>()
                .unwrap();
        patch(&mut doc);

        let out = doc.to_string();
        assert!(out.contains("fonts = { size = 12.0, hinting = true }"));
        let config: crate::Config = toml::from_str(&out).unwrap();
        assert_eq!(config.appearance, sample_appearance());
    }

    #[test]
    fn save_settings_to_creates_updates_and_rejects_invalid() {
        let dir = std::env::temp_dir().join("NiumaTerm-settings-save-test");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("config.toml");

        let save = || {
            save_settings_to(
                &path,
                "test-theme",
                &sample_appearance(),
                &sample_agent(),
                &sample_system(),
                &sample_remote_session(),
                &sample_profiles(),
                "PowerShell",
            )
        };

        // Missing dir + file: created from scratch.
        save().unwrap();
        let config: crate::Config =
            toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(config.appearance, sample_appearance());
        assert_eq!(config.agent, sample_agent());
        assert_eq!(config.theme, "test-theme");

        // Second save updates in place and leaves no temp file behind.
        save().unwrap();
        let config: crate::Config =
            toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(config.profiles.default, "PowerShell");
        assert!(!path.with_extension("toml.tmp").exists());

        // Invalid TOML: save refuses and the file is left untouched.
        std::fs::write(&path, "not [ valid").unwrap();
        assert!(save().is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "not [ valid");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
