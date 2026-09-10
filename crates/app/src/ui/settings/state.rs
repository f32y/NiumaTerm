use std::io;
use std::path::Path;

use gpui::Global;
#[cfg(windows)]
use gpui::SharedString;
use nmt_agent_utils::deepseek;
use nmt_app_agent::AgentKind;
use nmt_config::agent::AgentConfig;
pub use nmt_config::agent::{CollapseRows, ModelListStyle};
use nmt_config::appearance::AppearanceConfig;
pub use nmt_config::appearance::{InputStyle, Language, TabBarStyle, WindowBackdrop};
use nmt_config::defaults::default_theme;
pub use nmt_config::profile::{
    AgentProfile, AgentProfileKind, AgentProfileLauncher, EnvVar, Profile,
};
use nmt_config::remote_session::RemoteSessionConfig;
use nmt_config::system::SystemConfig;
use nmt_config::theme::Theme;
use nmt_config::update::UpdateConfig;
use nmt_config::{Config, CursorShape, SettingsPatch, config_file_path, get, save_settings_to};
use nmt_i18n::i18n;

use crate::ui::settings::MAX_TAB_WIDTH;

/// The shell a freshly seeded profile names, which is the platform's own
/// default rather than a fixed program.
#[cfg(test)]
pub fn default_shell_for_tests() -> String {
    default_shell()
}
/// The fixed-pitch face the terminal grid falls back to, and the proportional
/// one the interface does. Both mirror the configuration defaults; see
/// `nmt_config::appearance` for why macOS names its system face through a
/// token rather than by family.
#[cfg(target_os = "windows")]
pub const DEFAULT_FONT_FAMILY: &str = "Consolas";
#[cfg(target_os = "macos")]
pub const DEFAULT_FONT_FAMILY: &str = "Menlo";
#[cfg(all(unix, not(target_os = "macos")))]
pub const DEFAULT_FONT_FAMILY: &str = "monospace";
pub const DEFAULT_FONT_SIZE: f64 = 14.0;
pub const DEFAULT_AGENT_TRANSCRIPT_FONT_SIZE: f64 = 13.0;
pub const DEFAULT_LINE_HEIGHT: f64 = 1.0;
pub(super) const DEFAULT_BACKGROUND_IMAGE_OPACITY: f64 = 0.3;
#[cfg(target_os = "windows")]
pub const DEFAULT_UI_FONT: &str = "Segoe UI";
#[cfg(not(target_os = "windows"))]
pub const DEFAULT_UI_FONT: &str = ".SystemUIFont";
pub const MIN_TAB_WIDTH: f64 = 120.0;
pub const DEFAULT_TAB_WIDTH: f64 = 220.0;

/// Persistent settings are shared with the configuration reader and writer.
/// Picker state and save errors live separately and never enter a pane snapshot.
pub struct AppSettings {
    /// File stem selected from the per-user themes directory.
    pub theme: String,
    pub appearance: AppearanceConfig,
    pub agent: AgentConfig,
    pub system: SystemConfig,
    pub remote_session: RemoteSessionConfig,
    pub update: UpdateConfig,
    pub cursor_shape: CursorShape,
    pub profiles: Vec<Profile>,
    /// Resolves by name; loading and profile edits repair dangling references.
    pub default_profile: String,
    pub agent_profiles: Vec<AgentProfile>,
    /// Empty when the user has deliberately removed every agent profile.
    pub default_agent_profile: String,
    pub editing: SettingsEditing,
}

#[derive(Default)]
pub struct SettingsEditing {
    pub theme_filter: String,
    /// Parsed theme files refreshed by the settings surface's watcher.
    pub themes: Vec<(String, Theme)>,
    /// Cleared only after another save succeeds.
    pub save_error: Option<String>,
    /// The last window's explicit choice must also bypass the final quit hook.
    pub discard_on_exit: bool,
    #[cfg(windows)]
    pub remote_pairing_code: Option<String>,
    #[cfg(windows)]
    pub remote_pairing_input: SharedString,
    #[cfg(windows)]
    pub remote_client_status: Option<String>,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            theme: String::new(),
            appearance: AppearanceConfig::default(),
            agent: AgentConfig::default(),
            system: SystemConfig::default(),
            remote_session: RemoteSessionConfig::default(),
            update: UpdateConfig::default(),
            cursor_shape: CursorShape::Block,
            profiles: vec![builtin_profile()],
            default_profile: builtin_profile().name,
            agent_profiles: builtin_agent_profiles(),
            default_agent_profile: agent_kind_label(AgentProfileKind::ClaudeCode).to_string(),
            editing: SettingsEditing::default(),
        }
    }
}

impl Global for AppSettings {}

pub(super) fn input_style_label(style: InputStyle) -> &'static str {
    match style {
        InputStyle::Waterfall => i18n("settings-terminal-input-style-waterfall"),
        InputStyle::FixedBottom => i18n("settings-terminal-input-style-fixed-bottom"),
    }
}

pub(super) fn input_style_from_value(value: &str) -> InputStyle {
    match value {
        "fixed-bottom" => InputStyle::FixedBottom,
        _ => InputStyle::Waterfall,
    }
}

pub(super) fn cursor_shape_from_value(value: &str) -> CursorShape {
    match value {
        "line" => CursorShape::Beam,
        "underline" => CursorShape::Underline,
        _ => CursorShape::Block,
    }
}

fn default_shell() -> String {
    nmt_platform::default_shell()
}

/// The built-in profile seeded when the config file defines none.
fn builtin_profile() -> Profile {
    Profile {
        name: "PowerShell".to_string(),
        shell: default_shell(),
        args: String::new(),
    }
}

/// Display name of a supported agent CLI, doubling as the seeded profile name.
pub(super) fn agent_kind_label(kind: AgentProfileKind) -> &'static str {
    match kind {
        AgentProfileKind::ClaudeCode => "Claude Code",
        AgentProfileKind::Codex => "Codex",
        AgentProfileKind::DeepSeek => "DeepSeek Harness",
    }
}

pub(super) fn agent_kind_display_label(kind: AgentProfileKind) -> &'static str {
    match kind {
        AgentProfileKind::ClaudeCode => i18n("settings-agent-kind-claude-code"),
        AgentProfileKind::Codex => i18n("settings-agent-kind-codex"),
        AgentProfileKind::DeepSeek => i18n("settings-agent-kind-deepseek"),
    }
}

/// The built-in agent profile for `kind`. The bare executable name resolves
/// through PATH (and PATHEXT on Windows), so it finds `claude.exe` as well as
/// the npm `claude.cmd` shim.
pub(crate) fn builtin_agent_profile(kind: AgentProfileKind) -> AgentProfile {
    let executable = match kind {
        AgentProfileKind::ClaudeCode => "claude",
        AgentProfileKind::Codex => "codex",
        AgentProfileKind::DeepSeek => deepseek::DEFAULT_EXECUTABLE,
    };

    AgentProfile {
        name: agent_kind_label(kind).to_string(),
        kind,
        executable: executable.to_string(),
        // DeepSeek Harness is published to npm and has no installer of its
        // own, so a fresh profile runs it through npx and needs nothing
        // installed first. The executable stays filled in as what the profile
        // falls back to once it is pointed at a binary instead.
        launcher: if kind == AgentProfileKind::DeepSeek {
            AgentProfileLauncher::Npx
        } else {
            AgentProfileLauncher::Custom
        },
        ..AgentProfile::default()
    }
}

/// The agent profiles seeded when the config file defines none: one per
/// harness. Reading the registered kinds is what keeps a newly added harness
/// from needing this list edited too.
fn builtin_agent_profiles() -> Vec<AgentProfile> {
    AgentKind::ALL
        .into_iter()
        .map(|kind| builtin_agent_profile(kind.profile_kind()))
        .collect()
}

/// Snap a persisted refresh interval to the allowed set, falling back to 30.
pub(super) fn clamp_git_interval(seconds: u64) -> u64 {
    if matches!(seconds, 10 | 15 | 30 | 60) {
        seconds
    } else {
        30
    }
}

/// The configured UI font, or the default when the config leaves it blank
/// (an empty family would fall back to gpui's default, not Segoe UI).
pub(super) fn ui_font_or_default(family: &str) -> String {
    if family.trim().is_empty() {
        DEFAULT_UI_FONT.into()
    } else {
        family.to_string()
    }
}

pub(super) fn terminal_font_or_default(family: &str) -> String {
    if family.trim().is_empty() {
        DEFAULT_FONT_FAMILY.into()
    } else {
        family.to_string()
    }
}

/// Clamp a persisted tab width to the allowed range, falling back to the
/// default for non-finite values.
pub(super) fn clamp_tab_width(width: f64) -> f64 {
    if width.is_finite() {
        width.clamp(MIN_TAB_WIDTH, MAX_TAB_WIDTH)
    } else {
        DEFAULT_TAB_WIDTH
    }
}

pub(super) fn clamp_terminal_font_size(size: f64) -> f64 {
    if size.is_finite() {
        size.clamp(6.0, 72.0)
    } else {
        DEFAULT_FONT_SIZE
    }
}

pub(super) fn clamp_agent_transcript_font_size(size: f64) -> f64 {
    if size.is_finite() {
        size.clamp(6.0, 72.0)
    } else {
        DEFAULT_AGENT_TRANSCRIPT_FONT_SIZE
    }
}

pub(super) fn clamp_terminal_line_height(line_height: f64) -> f64 {
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
pub(super) fn clamp_background_opacity(opacity: f64) -> f64 {
    clamp_opacity(opacity, 0.2, 1.0)
}

pub(super) fn clamp_background_image_opacity(opacity: f64) -> f64 {
    clamp_opacity(opacity, 0.0, DEFAULT_BACKGROUND_IMAGE_OPACITY)
}

impl AppSettings {
    pub fn load() -> Self {
        let config = get();

        let mut appearance = config.appearance.clone();

        let profiles: Vec<Profile> = if config.profiles.list.is_empty() {
            vec![builtin_profile()]
        } else {
            config.profiles.list.clone()
        };

        // An unset or dangling default falls back to the first profile.
        let default_profile = if profiles.iter().any(|p| p.name == config.profiles.default) {
            config.profiles.default.clone()
        } else {
            profiles[0].name.clone()
        };

        // Seed the built-ins only for a never-configured section; once the
        // dialog has saved (`initialized`), an empty list is a deliberate
        // "no agent profiles" state.
        let agent_profiles: Vec<AgentProfile> =
            if config.agent_profiles.list.is_empty() && !config.agent_profiles.initialized {
                builtin_agent_profiles()
            } else {
                config.agent_profiles.list.clone()
            };

        let default_agent_profile = if agent_profiles
            .iter()
            .any(|p| p.name == config.agent_profiles.default)
        {
            config.agent_profiles.default.clone()
        } else {
            agent_profiles
                .first()
                .map(|p| p.name.clone())
                .unwrap_or_default()
        };

        appearance.git_status_refresh_interval =
            clamp_git_interval(appearance.git_status_refresh_interval);
        appearance.ui_font = ui_font_or_default(&appearance.ui_font);
        appearance.terminal_font_family =
            terminal_font_or_default(&appearance.terminal_font_family);
        appearance.agent_font_family = ui_font_or_default(&appearance.agent_font_family);
        appearance.agent_transcript_font_family =
            terminal_font_or_default(&appearance.agent_transcript_font_family);
        appearance.terminal_font_size = clamp_terminal_font_size(appearance.terminal_font_size);
        appearance.agent_font_size = clamp_terminal_font_size(appearance.agent_font_size);
        appearance.agent_transcript_font_size =
            clamp_agent_transcript_font_size(appearance.agent_transcript_font_size);
        appearance.terminal_line_height =
            clamp_terminal_line_height(appearance.terminal_line_height);
        appearance.tab_width = clamp_tab_width(appearance.tab_width);
        appearance.background_opacity = clamp_background_opacity(appearance.background_opacity);
        appearance.background_image_opacity =
            clamp_background_image_opacity(appearance.background_image_opacity);
        appearance.background_image = appearance
            .background_image
            .filter(|path| !path.trim().is_empty());

        Self {
            theme: if config.theme.is_empty() {
                default_theme()
            } else {
                config.theme.clone()
            },
            appearance,
            agent: config.agent.clone(),
            system: config.system.clone(),
            remote_session: config.remote_session.clone(),
            update: config.update.clone(),
            cursor_shape: config.cursor.shape,
            profiles,
            default_profile,
            agent_profiles,
            default_agent_profile,
            editing: SettingsEditing {
                themes: Config::load_themes(),
                ..SettingsEditing::default()
            },
        }
    }

    /// Append a new profile with a unique placeholder name.
    pub fn add_profile(&mut self) {
        let mut n = self.profiles.len() + 1;

        let name = loop {
            let candidate = i18n("settings-profiles-new-name").replace("{n}", &n.to_string());
            if !self.profiles.iter().any(|p| p.name == candidate) {
                break candidate;
            }
            n += 1;
        };

        self.profiles.push(Profile {
            name,
            ..builtin_profile()
        });
    }

    /// Remove the profile at `ix`. Refuses the last one; removing the
    /// default falls the default back to the first remaining profile.
    pub fn remove_profile(&mut self, ix: usize) {
        if self.profiles.len() <= 1 || ix >= self.profiles.len() {
            return;
        }

        let removed = self.profiles.remove(ix);

        if self.default_profile == removed.name {
            self.default_profile = self.profiles[0].name.clone();
        }
    }

    /// Rename the profile at `ix`, keeping the default reference in sync.
    /// `ix` is captured by long-lived UI closures, so it can be stale after
    /// a profile was removed; out-of-range renames are ignored.
    pub fn rename_profile(&mut self, ix: usize, name: String) {
        let Some(profile) = self.profiles.get_mut(ix) else {
            return;
        };

        if profile.name == self.default_profile {
            self.default_profile = name.clone();
        }

        profile.name = name;
    }

    /// A profile name that collides with no existing agent profile
    /// (`exclude` skips the entry being edited): the trimmed `desired` name,
    /// the kind label when empty, plus a numeric suffix on collision. Names
    /// must stay unique — they key the default selector, tab persistence,
    /// and per-profile thread defaults.
    pub fn unique_agent_profile_name(
        &self,
        desired: &str,
        kind: AgentProfileKind,
        exclude: Option<usize>,
    ) -> String {
        let base = if desired.trim().is_empty() {
            agent_kind_label(kind)
        } else {
            desired.trim()
        };

        let taken = |name: &str| {
            self.agent_profiles
                .iter()
                .enumerate()
                .any(|(ix, p)| Some(ix) != exclude && p.name == name)
        };

        let mut n = 2;
        let mut name = base.to_string();
        while taken(&name) {
            name = format!("{base} {n}");
            n += 1;
        }
        name
    }

    /// Remove the agent profile at `ix`; removing the default falls the
    /// default back to the first remaining profile (or clears it when the
    /// list becomes empty).
    pub fn remove_agent_profile(&mut self, ix: usize) {
        if ix >= self.agent_profiles.len() {
            return;
        }

        let removed = self.agent_profiles.remove(ix);

        if self.default_agent_profile == removed.name {
            self.default_agent_profile = self
                .agent_profiles
                .first()
                .map(|p| p.name.clone())
                .unwrap_or_default();
        }
    }

    /// Replace the agent profile at `ix`, keeping the default reference in
    /// sync with a rename. Out-of-range updates are ignored (stale index
    /// after a removal).
    pub fn update_agent_profile(&mut self, ix: usize, profile: AgentProfile) {
        let Some(slot) = self.agent_profiles.get_mut(ix) else {
            return;
        };

        if slot.name == self.default_agent_profile {
            self.default_agent_profile = profile.name.clone();
        }

        *slot = profile;
    }

    /// The agent profile new agent tabs launch with: the default by name,
    /// falling back to the first profile, then to the built-in Claude Code
    /// profile if the list is somehow empty.
    pub fn default_agent_profile_entry(&self) -> AgentProfile {
        self.agent_profiles
            .iter()
            .find(|p| p.name == self.default_agent_profile)
            .or_else(|| self.agent_profiles.first())
            .cloned()
            .unwrap_or_else(|| builtin_agent_profile(AgentProfileKind::ClaudeCode))
    }

    /// The default profile's launch command: shell plus whitespace-split
    /// args. `None` shell when the profile list is somehow empty or the shell
    /// path is blank (the session falls back to its built-in default).
    pub fn default_profile_command(&self) -> (Option<String>, Vec<String>) {
        let profile = self
            .profiles
            .iter()
            .find(|p| p.name == self.default_profile)
            .or_else(|| self.profiles.first());

        match profile {
            Some(p) if !p.shell.trim().is_empty() => (
                Some(p.shell.trim().to_string()),
                p.args.split_whitespace().map(str::to_string).collect(),
            ),
            _ => (None, Vec::new()),
        }
    }

    pub fn profile_name_for_command(&self, shell: Option<&str>, args: &[String]) -> String {
        self.profiles
            .iter()
            .find(|profile| {
                let profile_shell =
                    (!profile.shell.trim().is_empty()).then(|| profile.shell.trim());

                profile_shell.is_some_and(|value| {
                    shell.is_some_and(|shell| value.eq_ignore_ascii_case(shell))
                }) && profile
                    .args
                    .split_whitespace()
                    .eq(args.iter().map(String::as_str))
            })
            .map(|profile| profile.name.clone())
            .unwrap_or_else(|| self.default_profile.clone())
    }

    /// Persist the current edits without replacing unrelated TOML content.
    /// On failure the edited values remain available for another attempt.
    pub fn save(&mut self) -> io::Result<()> {
        self.save_to(&config_file_path())
    }

    pub(super) fn save_to(&mut self, path: &Path) -> io::Result<()> {
        let result = save_settings_to(
            path,
            &SettingsPatch {
                theme: &self.theme,
                appearance: &self.appearance,
                cursor_shape: self.cursor_shape,
                agent: &self.agent,
                system: &self.system,
                remote_session: &self.remote_session,
                update: &self.update,
                profiles: &self.profiles,
                default_profile: &self.default_profile,
                agent_profiles: &self.agent_profiles,
                default_agent_profile: &self.default_agent_profile,
            },
        );
        self.editing.save_error = result.as_ref().err().map(ToString::to_string);
        result
    }
}
