pub use nmt_config::agent::{CollapseRows, ModelListStyle};
#[cfg(test)]
pub use nmt_config::appearance::{
    DEFAULT_AGENT_TRANSCRIPT_FONT_SIZE, DEFAULT_BACKGROUND_IMAGE_OPACITY, DEFAULT_FONT_FAMILY,
    DEFAULT_FONT_SIZE, DEFAULT_LINE_HEIGHT, DEFAULT_TAB_WIDTH, DEFAULT_UI_FONT,
    clamp_agent_transcript_font_size, clamp_background_image_opacity, clamp_background_opacity,
    clamp_git_interval, clamp_tab_width, clamp_terminal_font_size, clamp_terminal_line_height,
    terminal_font_or_default, ui_font_or_default,
};
pub use nmt_config::appearance::{InputStyle, MIN_TAB_WIDTH, TabBarStyle, WindowBackdrop};
pub use nmt_config::profile::{
    AgentProfile, AgentProfileKind, AgentProfileLauncher, EnvVar, Profile,
};

use std::borrow::Cow;
use std::io;
use std::path::Path;

use app::agent_tab::AgentKind;
use gpui::Global;
#[cfg(windows)]
use gpui::SharedString;
use nmt_agent::dsh;
use nmt_config::agent::AgentConfig;
use nmt_config::appearance::AppearanceConfig;
use nmt_config::defaults::default_theme;
#[cfg(windows)]
use nmt_config::remote_session::RemoteSessionConfig;
use nmt_config::system::SystemConfig;
use nmt_config::terminal::TerminalConfig;
use nmt_config::theme::Theme;
use nmt_config::update::UpdateConfig;
use nmt_config::{Config, CursorShape, SettingsPatch, config_file_path, get, save_settings_to};
use rust_i18n::t;

/// The shell a freshly seeded profile names, which is the platform's own
/// default rather than a fixed program.
#[cfg(test)]
pub fn default_shell_for_tests() -> String {
    default_shell()
}

/// Persistent settings are shared with the configuration reader and writer.
/// Picker state lives separately and never enters a pane snapshot.
pub struct AppSettings {
    config: Config,
    discard_on_exit: bool,
}

#[derive(Default)]
pub struct SettingsEditing {
    pub theme_filter: String,

    /// Parsed theme files refreshed by the settings surface's watcher.
    pub themes: Vec<(String, Theme)>,

    #[cfg(windows)]
    pub remote_pairing_code: Option<String>,
    #[cfg(windows)]
    pub remote_pairing_input: SharedString,
    #[cfg(windows)]
    pub remote_client_status: Option<String>,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self::from_config(Config::default())
    }
}

impl Global for AppSettings {}

pub(super) fn input_style_label(style: InputStyle) -> Cow<'static, str> {
    match style {
        InputStyle::Waterfall => t!("settings-terminal-input-style-waterfall"),
        InputStyle::FixedBottom => t!("settings-terminal-input-style-fixed-bottom"),
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

pub(super) fn agent_kind_display_label(kind: AgentProfileKind) -> Cow<'static, str> {
    match kind {
        AgentProfileKind::Claude => t!("settings-agent-kind-claude-code"),
        AgentProfileKind::Codex => t!("settings-agent-kind-codex"),
        AgentProfileKind::DeepSeek => t!("settings-agent-kind-deepseek"),
    }
}

/// The built-in agent profile for `kind`. The bare executable name resolves
/// through PATH (and PATHEXT on Windows), so it finds `claude.exe` as well as
/// the npm `claude.cmd` shim.
pub(crate) fn builtin_agent_profile(kind: AgentProfileKind) -> AgentProfile {
    let executable = match kind {
        AgentProfileKind::Claude => "claude",
        AgentProfileKind::Codex => "codex",
        AgentProfileKind::DeepSeek => dsh::DEFAULT_EXECUTABLE,
    };

    AgentProfile {
        name: kind.full_name().to_string(),
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
        .map(builtin_agent_profile)
        .collect()
}

impl AppSettings {
    /// The last window's explicit discard also bypasses the final quit hook.
    pub(crate) fn discard_on_exit(&mut self) {
        self.discard_on_exit = true;
    }

    pub(crate) fn should_save_on_exit(&self) -> bool {
        !self.discard_on_exit
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn load() -> Self {
        Self::from_config(get().clone())
    }

    pub(crate) fn from_config(mut config: Config) -> Self {
        config.appearance.normalize();

        if config.theme.is_empty() {
            config.theme = default_theme();
        }

        if config.profiles.list.is_empty() {
            config.profiles.list.push(builtin_profile());
        }

        if !config
            .profiles
            .list
            .iter()
            .any(|p| p.name == config.profiles.default)
        {
            config.profiles.default = config.profiles.list[0].name.clone();
        }

        // Only a never-configured list receives built-ins. A saved empty list
        // means the user deliberately removed every agent profile.
        if config.agent_profiles.list.is_empty() && !config.agent_profiles.initialized {
            config.agent_profiles.list = builtin_agent_profiles();
        }

        if !config
            .agent_profiles
            .list
            .iter()
            .any(|p| p.name == config.agent_profiles.default)
        {
            config.agent_profiles.default = config
                .agent_profiles
                .list
                .first()
                .map(|p| p.name.clone())
                .unwrap_or_default();
        }

        Self {
            config,
            discard_on_exit: false,
        }
    }

    pub fn edit_appearance(&mut self, edit: impl FnOnce(&mut AppearanceConfig)) {
        edit(&mut self.config.appearance);
        self.config.appearance.normalize();
    }

    pub fn edit_agent(&mut self, edit: impl FnOnce(&mut AgentConfig)) {
        edit(&mut self.config.agent);
    }

    pub fn edit_system(&mut self, edit: impl FnOnce(&mut SystemConfig)) {
        edit(&mut self.config.system);
    }

    #[cfg(windows)]
    pub fn edit_remote_session(&mut self, edit: impl FnOnce(&mut RemoteSessionConfig)) {
        edit(&mut self.config.remote_session);
    }

    pub fn edit_update(&mut self, edit: impl FnOnce(&mut UpdateConfig)) {
        edit(&mut self.config.update);
    }

    pub(super) fn edit_terminal(&mut self, edit: impl FnOnce(&mut TerminalConfig)) {
        edit(&mut self.config.terminal);
    }

    pub fn set_theme(&mut self, theme: String) {
        self.config.theme = theme;
    }

    pub fn set_cursor_shape(&mut self, shape: CursorShape) {
        self.config.cursor.shape = shape;
    }

    pub fn set_default_profile(&mut self, name: String) -> bool {
        if !self.config.profiles.list.iter().any(|p| p.name == name) {
            return false;
        }

        self.config.profiles.default = name;

        true
    }

    pub fn set_default_agent_profile(&mut self, name: String) -> bool {
        if !self
            .config
            .agent_profiles
            .list
            .iter()
            .any(|p| p.name == name)
        {
            return false;
        }

        self.config.agent_profiles.default = name;

        true
    }

    pub fn set_profile_shell(&mut self, ix: usize, shell: String) -> bool {
        let Some(profile) = self.config.profiles.list.get_mut(ix) else {
            return false;
        };

        profile.shell = shell;

        true
    }

    pub fn set_profile_args(&mut self, ix: usize, args: String) -> bool {
        let Some(profile) = self.config.profiles.list.get_mut(ix) else {
            return false;
        };

        profile.args = args;

        true
    }

    pub fn duplicate_agent_profile(&mut self, ix: usize) -> bool {
        let Some(mut profile) = self.config.agent_profiles.list.get(ix).cloned() else {
            return false;
        };

        profile.name = self.unique_agent_profile_name(&profile.name, profile.kind, None);
        self.config.agent_profiles.list.insert(ix + 1, profile);

        true
    }

    pub fn move_agent_profile(&mut self, from: usize, to: usize) -> bool {
        let profiles = &mut self.config.agent_profiles.list;

        if from == to || from >= profiles.len() || to >= profiles.len() {
            return false;
        }

        let profile = profiles.remove(from);

        profiles.insert(to, profile);

        true
    }

    /// Append a new profile with a unique placeholder name.
    pub fn add_profile(&mut self) {
        let mut n = self.config.profiles.list.len() + 1;

        let name = loop {
            let candidate = t!("settings-profiles-new-name", n = n).into_owned();

            if !self
                .config
                .profiles
                .list
                .iter()
                .any(|p| p.name == candidate)
            {
                break candidate;
            }

            n += 1;
        };

        self.config.profiles.list.push(Profile {
            name,
            ..builtin_profile()
        });
    }

    /// Remove the profile at `ix`. Refuses the last one; removing the
    /// default falls the default back to the first remaining profile.
    pub fn remove_profile(&mut self, ix: usize) -> bool {
        if self.config.profiles.list.len() <= 1 || ix >= self.config.profiles.list.len() {
            return false;
        }

        let removed = self.config.profiles.list.remove(ix);

        if self.config.profiles.default == removed.name {
            self.config.profiles.default = self.config.profiles.list[0].name.clone();
        }

        true
    }

    /// Rename the profile at `ix`, keeping the default reference in sync.
    /// `ix` is captured by long-lived UI closures, so it can be stale after
    /// a profile was removed; out-of-range renames are ignored.
    pub fn rename_profile(&mut self, ix: usize, name: String) -> bool {
        let Some(profile) = self.config.profiles.list.get_mut(ix) else {
            return false;
        };

        if profile.name == self.config.profiles.default {
            self.config.profiles.default = name.clone();
        }

        profile.name = name;

        true
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
            kind.full_name()
        } else {
            desired.trim()
        };

        let taken = |name: &str| {
            self.config
                .agent_profiles
                .list
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
    pub fn remove_agent_profile(&mut self, ix: usize) -> bool {
        if ix >= self.config.agent_profiles.list.len() {
            return false;
        }

        let removed = self.config.agent_profiles.list.remove(ix);

        if self.config.agent_profiles.default == removed.name {
            self.config.agent_profiles.default = self
                .config
                .agent_profiles
                .list
                .first()
                .map(|p| p.name.clone())
                .unwrap_or_default();
        }

        true
    }

    /// Store a dialog draft with a unique name and a valid default reference.
    /// A stale edit index leaves the current list unchanged.
    pub fn save_agent_profile(&mut self, target: Option<usize>, mut profile: AgentProfile) -> bool {
        if target.is_some_and(|ix| ix >= self.config.agent_profiles.list.len()) {
            return false;
        }

        profile.env.retain(|var| !var.name.trim().is_empty());
        profile.name = self.unique_agent_profile_name(&profile.name, profile.kind, target);

        match target {
            Some(ix) => {
                let slot = &mut self.config.agent_profiles.list[ix];

                if slot.name == self.config.agent_profiles.default {
                    self.config.agent_profiles.default = profile.name.clone();
                }

                *slot = profile;
            }

            None => {
                if self.config.agent_profiles.default.is_empty() {
                    self.config.agent_profiles.default = profile.name.clone();
                }

                self.config.agent_profiles.list.push(profile);
            }
        }

        true
    }

    /// The agent profile new agent tabs launch with: the default by name,
    /// falling back to the first profile, then to the built-in Claude Code
    /// profile if the list is somehow empty.
    pub fn default_agent_profile_entry(&self) -> AgentProfile {
        self.config
            .agent_profiles
            .list
            .iter()
            .find(|p| p.name == self.config.agent_profiles.default)
            .or_else(|| self.config.agent_profiles.list.first())
            .cloned()
            .unwrap_or_else(|| builtin_agent_profile(AgentProfileKind::Claude))
    }

    /// The default profile's launch command: shell plus whitespace-split
    /// args. `None` shell when the profile list is somehow empty or the shell
    /// path is blank (the session falls back to its built-in default).
    pub fn default_profile_command(&self) -> (Option<String>, Vec<String>) {
        let profile = self
            .config
            .profiles
            .list
            .iter()
            .find(|p| p.name == self.config.profiles.default)
            .or_else(|| self.config.profiles.list.first());

        match profile {
            Some(p) if !p.shell.trim().is_empty() => (
                Some(p.shell.trim().to_string()),
                p.args.split_whitespace().map(str::to_string).collect(),
            ),

            _ => (None, Vec::new()),
        }
    }

    pub fn profile_name_for_command(&self, shell: Option<&str>, args: &[String]) -> String {
        self.config
            .profiles
            .list
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
            .unwrap_or_else(|| self.config.profiles.default.clone())
    }

    /// Persist the current edits without replacing unrelated TOML content.
    /// On failure the edited values remain available for another attempt.
    pub fn save(&self) -> io::Result<()> {
        self.save_to(&config_file_path())
    }

    pub(super) fn save_to(&self, path: &Path) -> io::Result<()> {
        save_settings_to(
            path,
            &SettingsPatch {
                theme: &self.config.theme,
                appearance: &self.config.appearance,
                cursor_shape: self.config.cursor.shape,
                agent: &self.config.agent,
                system: &self.config.system,
                remote_session: &self.config.remote_session,
                update: &self.config.update,
                profiles: &self.config.profiles.list,
                default_profile: &self.config.profiles.default,
                agent_profiles: &self.config.agent_profiles.list,
                default_agent_profile: &self.config.agent_profiles.default,
                terminal: &self.config.terminal,
            },
        )
    }
}
