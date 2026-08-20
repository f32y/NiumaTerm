use gpui::{Global, SharedString};
use nmt_agent_utils::deepseek;
use nmt_config::agent::AgentConfig;
use nmt_config::appearance::{AppearanceConfig, SmoothScrollingMode};
pub use nmt_config::appearance::{InputStyle, Language, TabBarStyle, WindowBackdrop};
use nmt_config::defaults::default_theme;
pub use nmt_config::profile::{AgentProfile, AgentProfileKind, EnvVar, Profile};
use nmt_config::remote_session::RemoteSessionConfig;
use nmt_config::system::{NewlineShortcut, SystemConfig, WarnBeforeTerminatingShell};
use nmt_config::theme::Theme;
use nmt_config::{CursorShape, SettingsPatch, get, save_settings};
use nmt_i18n::i18n;
use tracing::warn;

use crate::agent::AgentKind;
use crate::ui::settings::MAX_TAB_WIDTH;
use crate::ui::settings::theme::load_theme_choices;

pub const DEFAULT_SHELL: &str = r"C:\WINDOWS\System32\WindowsPowerShell\v1.0\powershell.exe";
pub const DEFAULT_FONT_FAMILY: &str = "Consolas";
pub const DEFAULT_FONT_SIZE: f64 = 14.0;
pub const DEFAULT_LINE_HEIGHT: f64 = 1.0;
pub(super) const DEFAULT_BACKGROUND_IMAGE_OPACITY: f64 = 0.3;
pub const DEFAULT_UI_FONT: &str = "Segoe UI";
pub const DEFAULT_TAB_WIDTH: f64 = 120.0;

/// The app-wide settings model, stored as a gpui global.
pub struct AppSettings {
    /// Selected file stem in the per-user `themes` directory.
    pub theme: String,
    /// Ephemeral filter for the theme list; it is not persisted.
    pub theme_filter: String,
    /// Parsed theme files, refreshed when the themes directory changes.
    pub themes: Vec<(String, Theme)>,
    pub agent_pane_use_terminal_background: bool,
    pub input_style: InputStyle,
    /// Move a scrolled terminal viewport to the latest output on typed input.
    pub scroll_to_bottom_when_typing: bool,
    pub cursor_shape: CursorShape,
    pub profiles: Vec<Profile>,
    /// Name of the profile new terminals use. Always references an existing
    /// profile by name (seeded to the first profile when unset).
    pub default_profile: String,
    /// Launch profiles for agent tabs (executable, endpoint, env vars).
    pub agent_profiles: Vec<AgentProfile>,
    /// Name of the agent profile new agent tabs use. Always references an
    /// existing profile by name (seeded to the first profile when unset).
    pub default_agent_profile: String,
    /// Render command blocks as a split frozen-history list.
    pub command_blocks: bool,
    /// Show today's ccusage token totals in the titlebar.
    pub show_daily_token_usage: bool,
    /// Show the git `+added -removed` line counts in the titlebar.
    pub show_git_status_on_title_bar: bool,
    /// Seconds between git status refreshes; always one of 10/15/30/60.
    pub git_status_refresh_interval: u64,
    /// Font family for the app chrome (titlebar, sidebar, tabs, dialogs).
    pub ui_font_family: SharedString,
    /// Font family used by the terminal view.
    pub terminal_font_family: SharedString,
    /// Font size (px) used by the terminal view.
    pub terminal_font_size: f64,
    /// Line height as a multiplier on font size.
    pub terminal_line_height: f64,
    /// Font family used by agent (chat) tabs.
    pub agent_font_family: SharedString,
    /// Font size (px) used by agent (chat) tabs.
    pub agent_font_size: f64,
    /// Fixed tab width in pixels (DEFAULT_TAB_WIDTH..=MAX_TAB_WIDTH). Ignored
    /// while `tab_auto_size` is on.
    pub tab_width: f64,
    /// Shrink tabs toward a minimum as the strip fills, rather than holding
    /// `tab_width` and overflowing into the strip's horizontal scroll.
    pub tab_auto_size: bool,
    /// Tab strip placement: a title-bar row, or rows nested under each
    /// workspace in the sidebar.
    pub tab_bar_style: TabBarStyle,
    /// Filter the settings font picker to monospace fonts.
    pub monospace_only: bool,
    /// Window backdrop material: Mica, Acrylic, or Off (see
    /// [`WindowBackdrop`]). Only Off forces an opaque window.
    pub window_backdrop: WindowBackdrop,
    /// Allow the Terminal View and Agent Pane background to remain translucent.
    pub transparent_main_view: bool,
    /// Select which scrolling views animate line-based mouse-wheel input.
    pub smooth_scrolling: SmoothScrollingMode,
    /// Whole-window background opacity (0.2..=1.0) while transparency is enabled.
    pub background_opacity: f64,
    /// Local image drawn behind all window content.
    pub background_image: Option<String>,
    /// How strongly the image shows through the window surfaces (0.0..=1.0).
    pub background_image_opacity: f64,
    /// UI display language.
    pub language: Language,
    /// Process lifecycle events received from Agent Hook executables.
    pub enable_agent_hooks: bool,
    /// Show Agent account usage in the workspace sidebar.
    pub show_agent_usage: bool,
    /// Collapse consecutive tool-call rows in agent tabs into a one-line
    /// summary by default.
    pub collapse_tool_calls: bool,
    /// Probe each Agent installation for a newer provider version in the
    /// background.
    pub check_agent_updates: bool,
    /// List Codex skills in the `/` command palette and rewrite a chosen one
    /// to its `$name` form.
    pub codex_skill_command_compat: bool,
    /// Restore the last saved workspace/tab session on startup.
    pub restore_last_session_when_opening: bool,
    /// Manage each tab's shell with a Windows Job Object: closing the tab
    /// kills the shell's entire process tree. Applies to new tabs.
    pub manage_subprocess_job: bool,
    /// When to warn before closing a shell.
    pub warn_before_terminating_shell: WarnBeforeTerminatingShell,
    /// Ask for confirmation before closing a workspace, Agent tab, or window.
    pub confirm_before_closing: bool,
    /// Raise the main (UI) and render thread priority to AboveNormal.
    pub prioritize_ui_threads: bool,
    /// Modified Enter key that inserts a new line without submitting input.
    pub newline_shortcut: NewlineShortcut,
    /// Open a directory in the deepest workspace that already contains it,
    /// instead of always opening a workspace of its own.
    pub open_in_best_workspace: bool,
    /// Host this machine's local sessions for remote clients via the relay.
    pub remote_host_enabled: bool,
    /// Relay endpoint both host and clients dial.
    pub remote_relay_url: SharedString,
    /// Shared token the relay requires from hosts on registration.
    pub remote_access_token: SharedString,
    /// Most recently generated pairing code, shown until the dialog closes.
    /// Ephemeral: never persisted.
    pub remote_pairing_code: Option<String>,
    /// Client-side: pairing code being entered to pair with a remote host.
    /// Ephemeral.
    pub remote_pairing_input: SharedString,
    /// Client-side: last pairing attempt result message. Ephemeral.
    pub remote_client_status: Option<String>,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            theme: String::new(),
            theme_filter: String::new(),
            themes: Vec::new(),
            agent_pane_use_terminal_background: false,
            input_style: InputStyle::Waterfall,
            scroll_to_bottom_when_typing: true,
            cursor_shape: CursorShape::Block,
            profiles: vec![builtin_profile()],
            default_profile: builtin_profile().name,
            agent_profiles: builtin_agent_profiles(),
            default_agent_profile: agent_kind_label(AgentProfileKind::ClaudeCode).to_string(),
            command_blocks: true,
            show_daily_token_usage: false,
            show_git_status_on_title_bar: false,
            git_status_refresh_interval: 30,
            ui_font_family: DEFAULT_UI_FONT.into(),
            terminal_font_family: initial_font_family(),
            terminal_font_size: DEFAULT_FONT_SIZE,
            terminal_line_height: DEFAULT_LINE_HEIGHT,
            agent_font_family: initial_font_family(),
            agent_font_size: DEFAULT_FONT_SIZE,
            tab_width: DEFAULT_TAB_WIDTH,
            tab_auto_size: false,
            tab_bar_style: TabBarStyle::default(),
            monospace_only: true,
            window_backdrop: WindowBackdrop::Acrylic,
            transparent_main_view: true,
            smooth_scrolling: SmoothScrollingMode::All,
            background_opacity: 1.0,
            background_image: None,
            background_image_opacity: DEFAULT_BACKGROUND_IMAGE_OPACITY,
            language: Language::default(),
            enable_agent_hooks: true,
            show_agent_usage: true,
            collapse_tool_calls: false,
            check_agent_updates: true,
            codex_skill_command_compat: true,
            restore_last_session_when_opening: true,
            manage_subprocess_job: false,
            warn_before_terminating_shell: WarnBeforeTerminatingShell::default(),
            confirm_before_closing: true,
            prioritize_ui_threads: false,
            newline_shortcut: NewlineShortcut::default(),
            open_in_best_workspace: true,
            remote_host_enabled: false,
            remote_relay_url: SharedString::default(),
            remote_access_token: SharedString::default(),
            remote_pairing_code: None,
            remote_pairing_input: SharedString::default(),
            remote_client_status: None,
        }
    }
}

impl Global for AppSettings {}

/// Initial terminal font. Live changes then go through
/// `AppSettings.terminal_font_family`.
fn initial_font_family() -> SharedString {
    DEFAULT_FONT_FAMILY.into()
}

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

/// The built-in profile seeded when the config file defines none.
fn builtin_profile() -> Profile {
    Profile {
        name: "PowerShell".to_string(),
        shell: DEFAULT_SHELL.to_string(),
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
pub(super) fn ui_font_or_default(family: &str) -> SharedString {
    if family.trim().is_empty() {
        DEFAULT_UI_FONT.into()
    } else {
        family.to_string().into()
    }
}

pub(super) fn terminal_font_or_default(family: &str) -> SharedString {
    if family.trim().is_empty() {
        DEFAULT_FONT_FAMILY.into()
    } else {
        family.to_string().into()
    }
}

/// Clamp a persisted tab width to the allowed range, falling back to the
/// default for non-finite values.
pub(super) fn clamp_tab_width(width: f64) -> f64 {
    if width.is_finite() {
        width.clamp(DEFAULT_TAB_WIDTH, MAX_TAB_WIDTH)
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

        let appearance = &config.appearance;

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

        Self {
            theme: if config.theme.is_empty() {
                default_theme()
            } else {
                config.theme.clone()
            },
            theme_filter: String::new(),
            themes: load_theme_choices(),
            agent_pane_use_terminal_background: appearance.agent_pane_use_terminal_background,
            input_style: appearance.input_style,
            scroll_to_bottom_when_typing: appearance.scroll_to_bottom_when_typing,
            cursor_shape: config.cursor.shape,
            profiles,
            default_profile,
            agent_profiles,
            default_agent_profile,
            command_blocks: appearance.command_blocks,
            show_daily_token_usage: appearance.show_daily_token_usage,
            show_git_status_on_title_bar: appearance.show_git_status_on_title_bar,
            git_status_refresh_interval: clamp_git_interval(appearance.git_status_refresh_interval),
            ui_font_family: ui_font_or_default(&appearance.ui_font),
            terminal_font_family: terminal_font_or_default(&appearance.terminal_font_family),
            terminal_font_size: clamp_terminal_font_size(appearance.terminal_font_size),
            terminal_line_height: clamp_terminal_line_height(appearance.terminal_line_height),
            agent_font_family: terminal_font_or_default(&appearance.agent_font_family),
            agent_font_size: clamp_terminal_font_size(appearance.agent_font_size),
            tab_width: clamp_tab_width(appearance.tab_width),
            tab_auto_size: appearance.tab_auto_size,
            tab_bar_style: appearance.tab_bar_style,
            monospace_only: appearance.monospace_only,
            window_backdrop: appearance.window_backdrop,
            transparent_main_view: appearance.transparent_main_view,
            smooth_scrolling: appearance.smooth_scrolling,
            background_opacity: clamp_background_opacity(appearance.background_opacity),
            background_image: appearance
                .background_image
                .clone()
                .filter(|path| !path.trim().is_empty()),
            background_image_opacity: clamp_background_image_opacity(
                appearance.background_image_opacity,
            ),
            language: appearance.language,
            enable_agent_hooks: config.agent.enable_agent_hooks,
            show_agent_usage: config.agent.show_agent_usage,
            collapse_tool_calls: config.agent.collapse_tool_calls,
            check_agent_updates: config.agent.check_agent_updates,
            codex_skill_command_compat: config.agent.codex_skill_command_compat,
            restore_last_session_when_opening: config.system.restore_last_session_when_opening,
            manage_subprocess_job: config.system.manage_subprocess_job,
            warn_before_terminating_shell: config.system.warn_before_terminating_shell,
            confirm_before_closing: config.system.confirm_before_closing_workspace,
            prioritize_ui_threads: config.system.prioritize_ui_threads,
            newline_shortcut: config.system.newline_shortcut,
            open_in_best_workspace: config.system.open_in_best_workspace,
            remote_host_enabled: config.remote_session.host_enabled,
            remote_relay_url: config.remote_session.relay_url.clone().into(),
            remote_access_token: config.remote_session.access_token.clone().into(),
            remote_pairing_code: None,
            remote_pairing_input: SharedString::default(),
            remote_client_status: None,
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

    pub(super) fn appearance_config(&self) -> AppearanceConfig {
        AppearanceConfig {
            input_style: self.input_style,
            scroll_to_bottom_when_typing: self.scroll_to_bottom_when_typing,
            agent_pane_use_terminal_background: self.agent_pane_use_terminal_background,
            command_blocks: self.command_blocks,
            show_daily_token_usage: self.show_daily_token_usage,
            show_git_status_on_title_bar: self.show_git_status_on_title_bar,
            git_status_refresh_interval: self.git_status_refresh_interval,
            tab_width: self.tab_width,
            tab_auto_size: self.tab_auto_size,
            tab_bar_style: self.tab_bar_style,
            ui_font: self.ui_font_family.to_string(),
            terminal_font_family: self.terminal_font_family.to_string(),
            terminal_font_size: self.terminal_font_size,
            terminal_line_height: self.terminal_line_height,
            agent_font_family: self.agent_font_family.to_string(),
            agent_font_size: self.agent_font_size,
            monospace_only: self.monospace_only,
            window_backdrop: self.window_backdrop,
            transparent_main_view: self.transparent_main_view,
            smooth_scrolling: self.smooth_scrolling,
            background_opacity: self.background_opacity,
            background_image: self.background_image.clone(),
            background_image_opacity: self.background_image_opacity,
            language: self.language,
        }
    }

    /// Persist the dialog-managed settings into `config.toml` (patch-style,
    /// preserving unrelated content). Called once on dialog close. Failures are
    /// logged, never fatal.
    pub fn save(&self) {
        let appearance = self.appearance_config();

        let agent = AgentConfig {
            enable_agent_hooks: self.enable_agent_hooks,
            show_agent_usage: self.show_agent_usage,
            collapse_tool_calls: self.collapse_tool_calls,
            check_agent_updates: self.check_agent_updates,
            codex_skill_command_compat: self.codex_skill_command_compat,
        };

        let system = SystemConfig {
            restore_last_session_when_opening: self.restore_last_session_when_opening,
            manage_subprocess_job: self.manage_subprocess_job,
            warn_before_terminating_shell: self.warn_before_terminating_shell,
            confirm_before_closing_workspace: self.confirm_before_closing,
            prioritize_ui_threads: self.prioritize_ui_threads,
            newline_shortcut: self.newline_shortcut,
            open_in_best_workspace: self.open_in_best_workspace,
        };

        let remote_session = RemoteSessionConfig {
            host_enabled: self.remote_host_enabled,
            relay_url: self.remote_relay_url.to_string(),
            access_token: self.remote_access_token.to_string(),
        };

        let profiles = self.profiles.clone();

        if let Err(err) = save_settings(&SettingsPatch {
            theme: &self.theme,
            appearance: &appearance,
            cursor_shape: self.cursor_shape,
            agent: &agent,
            system: &system,
            remote_session: &remote_session,
            profiles: &profiles,
            default_profile: &self.default_profile,
            agent_profiles: &self.agent_profiles,
            default_agent_profile: &self.default_agent_profile,
        }) {
            warn!("failed to save settings to config.toml: {err}");
        }
    }
}
