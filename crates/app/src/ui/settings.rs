//! Persisted to `config.toml`: seeded via [`AppSettings::load`] at startup,
//! written back patch-style via [`AppSettings::save`] once when the settings
//! dialog closes (see `Shell::on_show_settings`). Field edits mutate the global
//! live for preview; only closing the dialog persists them.

use std::rc::Rc;
use std::{fs, io, path};

use futures::StreamExt as _;
use futures::channel::mpsc::unbounded;
use gpui::prelude::{FluentBuilder as _, InteractiveElement as _, StatefulInteractiveElement as _};
use gpui::{
    AnyElement, App, AppContext as _, BorrowAppContext as _, ClipboardItem, Div, Entity,
    FileDialogFilter, Global, Hsla, IntoElement as _, ParentElement as _, PathPromptOptions,
    SharedString, StyleRefinement, Styled as _, Subscription, Task, Window,
    WindowBackgroundAppearance, div, px, relative, rgba,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::dialog::{DialogClose, DialogFooter};
use gpui_component::group_box::{GroupBox, GroupBoxVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::label::Label;
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::setting::{
    NumberFieldOptions, SettingField, SettingGroup, SettingItem, SettingPage, Settings,
};
use gpui_component::slider::{Slider, SliderEvent, SliderState};
use gpui_component::switch::Switch;
use gpui_component::{
    ActiveTheme as _, AxisExt as _, Disableable as _, Sizable as _, Theme as ComponentTheme,
    ThemeConfig as ComponentThemeConfig, ThemeRegistry as ComponentThemeRegistry,
    ThemeToken as ComponentThemeToken, WindowExt as _, h_flex, v_flex,
};
use nmt_agent_utils::HookInstallStatus;
use nmt_agent_utils::claude_code::hook as claude_hook;
use nmt_agent_utils::codex::hook as codex_hook;
use nmt_config::agent::AgentConfig;
use nmt_config::appearance::AppearanceConfig;
use nmt_config::colors::{ColorArray, Colors};
use nmt_config::defaults::default_theme;
use nmt_config::remote_session::RemoteSessionConfig;
use nmt_config::system::{SystemConfig, WarnBeforeTerminatingShell};
use nmt_config::theme::{AppearanceTheme, Theme, UiTheme};
use nmt_config::{
    Config, CursorShape, SettingsPatch, config_dir_path, get, save_settings, set_active_colors,
};
use nmt_platform::{
    is_shell_integration_registered, register_shell_integration, set_system_notification_enabled,
    shell_integration_dll_mismatched, system_notification_enabled, unregister_shell_integration,
};
use notify::{
    Event as NotifyEvent, RecursiveMode, Result as NotifyResult, Watcher as _, recommended_watcher,
};
use toml::{Table as TomlTable, Value as TomlValue};
use tracing::warn;

use crate::{PlatformHandle, remote, ui};

pub const DEFAULT_SHELL: &str = r"C:\WINDOWS\System32\WindowsPowerShell\v1.0\powershell.exe";

pub const DEFAULT_FONT_FAMILY: &str = "Consolas";
pub const DEFAULT_FONT_SIZE: f64 = 14.0;
pub const DEFAULT_LINE_HEIGHT: f64 = 1.0;
const DEFAULT_BACKGROUND_IMAGE_OPACITY: f64 = 0.3;

pub const DEFAULT_UI_FONT: &str = "Segoe UI";

pub const DEFAULT_TAB_WIDTH: f64 = 120.0;
pub const MAX_TAB_WIDTH: f64 = DEFAULT_TAB_WIDTH * 3.0;

/// Initial terminal font. Live changes then go through
/// `AppSettings.terminal_font_family`.
fn initial_font_family() -> SharedString {
    DEFAULT_FONT_FAMILY.into()
}

pub use nmt_config::appearance::InputStyle;
pub use nmt_config::profile::{AgentProfile, AgentProfileKind, EnvVar, Profile};

fn input_style_label(style: InputStyle) -> &'static str {
    match style {
        InputStyle::Waterfall => "Waterfall",
        InputStyle::FixedBottom => "Fixed Bottom",
    }
}

fn input_style_from_value(value: &str) -> InputStyle {
    match value {
        "fixed-bottom" => InputStyle::FixedBottom,
        _ => InputStyle::Waterfall,
    }
}

fn cursor_shape_from_value(value: &str) -> CursorShape {
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
fn agent_kind_label(kind: AgentProfileKind) -> &'static str {
    match kind {
        AgentProfileKind::ClaudeCode => "Claude Code",
        AgentProfileKind::Codex => "Codex",
    }
}

/// The built-in agent profile for `kind`. The bare executable name resolves
/// through PATH (and PATHEXT on Windows), so it finds `claude.exe` as well as
/// the npm `claude.cmd` shim.
pub(crate) fn builtin_agent_profile(kind: AgentProfileKind) -> AgentProfile {
    let executable = match kind {
        AgentProfileKind::ClaudeCode => "claude",
        AgentProfileKind::Codex => "codex",
    };

    AgentProfile {
        name: agent_kind_label(kind).to_string(),
        kind,
        executable: executable.to_string(),
        ..AgentProfile::default()
    }
}

/// The agent profiles seeded when the config file defines none: one per
/// supported agent CLI.
fn builtin_agent_profiles() -> Vec<AgentProfile> {
    vec![
        builtin_agent_profile(AgentProfileKind::ClaudeCode),
        builtin_agent_profile(AgentProfileKind::Codex),
    ]
}

/// The app-wide settings model, stored as a gpui global.
pub struct AppSettings {
    /// Selected file stem in the per-user `themes` directory.
    pub theme: String,
    /// Ephemeral filter for the theme list; it is not persisted.
    pub theme_filter: String,
    /// Parsed theme files, refreshed when the themes directory changes.
    pub themes: Vec<(String, Theme)>,
    pub input_style: InputStyle,
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
    /// Fixed tab width in pixels (DEFAULT_TAB_WIDTH..=MAX_TAB_WIDTH).
    pub tab_width: f64,
    /// Filter the settings font picker to monospace fonts.
    pub monospace_only: bool,
    /// Whether windows use an alpha-capable render target and acrylic backdrop.
    pub window_transparency_enabled: bool,
    /// Whole-window background opacity (0.2..=1.0) while transparency is enabled.
    pub background_opacity: f64,
    /// Local image drawn behind all window content.
    pub background_image: Option<String>,
    /// How strongly the image shows through the window surfaces (0.0..=1.0).
    pub background_image_opacity: f64,
    /// Process lifecycle events received from Agent Hook executables.
    pub enable_agent_hooks: bool,
    /// Show Agent account usage in the workspace sidebar.
    pub show_agent_usage: bool,
    /// Collapse consecutive tool-call rows in agent tabs into a one-line
    /// summary by default.
    pub collapse_tool_calls: bool,
    /// Restore the last saved workspace/tab session on startup.
    pub restore_last_session_when_opening: bool,
    /// Manage each tab's shell with a Windows Job Object: closing the tab
    /// kills the shell's entire process tree. Applies to new tabs.
    pub manage_subprocess_job: bool,
    /// When to warn before closing a shell.
    pub warn_before_terminating_shell: WarnBeforeTerminatingShell,
    /// Ask for confirmation before closing a workspace.
    pub confirm_before_closing_workspace: bool,
    /// Raise the main (UI) and render thread priority to AboveNormal.
    pub prioritize_ui_threads: bool,
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
            input_style: InputStyle::Waterfall,
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
            monospace_only: true,
            window_transparency_enabled: true,
            background_opacity: 1.0,
            background_image: None,
            background_image_opacity: DEFAULT_BACKGROUND_IMAGE_OPACITY,
            enable_agent_hooks: true,
            show_agent_usage: true,
            collapse_tool_calls: false,
            restore_last_session_when_opening: true,
            manage_subprocess_job: false,
            warn_before_terminating_shell: WarnBeforeTerminatingShell::default(),
            confirm_before_closing_workspace: true,
            prioritize_ui_threads: false,
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

/// Draft edited in the agent-profile dialog: `target` is the list index in
/// edit mode, `None` while adding. Inputs write here; only Save commits the
/// draft into `AppSettings`, so Cancel is a plain close.
#[derive(Default)]
struct AgentProfileDraft {
    target: Option<usize>,
    profile: AgentProfile,
}

impl Global for AgentProfileDraft {}

/// Persistent input state for a text field inside a profile card, created
/// via `window.use_keyed_state` so it survives the per-frame settings-view
/// rebuild. The subscription writes edits back into the `AppSettings` global.
struct CardInputState {
    input: Entity<InputState>,
    _subscription: Subscription,
}

/// A window-keyed text input bound to a value in the `AppSettings` global.
/// `apply` receives the new text on every change. When the backing value
/// changes underneath a reused key (e.g. a profile removal shifts indices),
/// the sync below rewrites the input to match.
fn card_text_input(
    key: String,
    value: SharedString,
    masked: bool,
    apply: impl Fn(String, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) -> Entity<InputState> {
    let state = window.use_keyed_state(SharedString::from(key), cx, {
        let value = value.clone();

        move |window, cx| {
            let input = cx.new(|cx| {
                InputState::new(window, cx)
                    .masked(masked)
                    .default_value(value)
            });

            let _subscription = cx.subscribe(&input, move |_, input, event, cx| {
                if matches!(event, InputEvent::Change) {
                    let value = input.read(cx).value().to_string();
                    apply(value, cx);
                }
            });

            CardInputState {
                input,
                _subscription,
            }
        }
    });

    let input = state.read(cx).input.clone();

    if input.read(cx).value() != value {
        input.update(cx, |input, cx| {
            input.set_value(value.clone(), window, cx);
        });
    }

    input
}

/// Snap a persisted refresh interval to the allowed set, falling back to 30.
fn clamp_git_interval(seconds: u64) -> u64 {
    if matches!(seconds, 10 | 15 | 30 | 60) {
        seconds
    } else {
        30
    }
}

/// The configured UI font, or the default when the config leaves it blank
/// (an empty family would fall back to gpui's default, not Segoe UI).
fn ui_font_or_default(family: &str) -> SharedString {
    if family.trim().is_empty() {
        DEFAULT_UI_FONT.into()
    } else {
        family.to_string().into()
    }
}

fn terminal_font_or_default(family: &str) -> SharedString {
    if family.trim().is_empty() {
        DEFAULT_FONT_FAMILY.into()
    } else {
        family.to_string().into()
    }
}

/// Clamp a persisted tab width to the allowed range, falling back to the
/// default for non-finite values.
fn clamp_tab_width(width: f64) -> f64 {
    if width.is_finite() {
        width.clamp(DEFAULT_TAB_WIDTH, MAX_TAB_WIDTH)
    } else {
        DEFAULT_TAB_WIDTH
    }
}

fn clamp_terminal_font_size(size: f64) -> f64 {
    if size.is_finite() {
        size.clamp(6.0, 72.0)
    } else {
        DEFAULT_FONT_SIZE
    }
}

fn clamp_terminal_line_height(line_height: f64) -> f64 {
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
fn clamp_background_opacity(opacity: f64) -> f64 {
    clamp_opacity(opacity, 0.2, 1.0)
}

fn clamp_background_image_opacity(opacity: f64) -> f64 {
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
            input_style: appearance.input_style,
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
            monospace_only: appearance.monospace_only,
            window_transparency_enabled: appearance.window_transparency_enabled,
            background_opacity: clamp_background_opacity(appearance.background_opacity),
            background_image: appearance
                .background_image
                .clone()
                .filter(|path| !path.trim().is_empty()),
            background_image_opacity: clamp_background_image_opacity(
                appearance.background_image_opacity,
            ),
            enable_agent_hooks: config.agent.enable_agent_hooks,
            show_agent_usage: config.agent.show_agent_usage,
            collapse_tool_calls: config.agent.collapse_tool_calls,
            restore_last_session_when_opening: config.system.restore_last_session_when_opening,
            manage_subprocess_job: config.system.manage_subprocess_job,
            warn_before_terminating_shell: config.system.warn_before_terminating_shell,
            confirm_before_closing_workspace: config.system.confirm_before_closing_workspace,
            prioritize_ui_threads: config.system.prioritize_ui_threads,
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
            let candidate = format!("Profile {n}");
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

    /// Persist the dialog-managed settings into `config.toml` (patch-style,
    /// preserving unrelated content). Called once on dialog close. Failures are
    /// logged, never fatal.
    pub fn save(&self) {
        let appearance = AppearanceConfig {
            input_style: self.input_style,
            command_blocks: self.command_blocks,
            show_daily_token_usage: self.show_daily_token_usage,
            show_git_status_on_title_bar: self.show_git_status_on_title_bar,
            git_status_refresh_interval: self.git_status_refresh_interval,
            tab_width: self.tab_width,
            ui_font: self.ui_font_family.to_string(),
            terminal_font_family: self.terminal_font_family.to_string(),
            terminal_font_size: self.terminal_font_size,
            terminal_line_height: self.terminal_line_height,
            agent_font_family: self.agent_font_family.to_string(),
            agent_font_size: self.agent_font_size,
            monospace_only: self.monospace_only,
            window_transparency_enabled: self.window_transparency_enabled,
            background_opacity: self.background_opacity,
            background_image: self.background_image.clone(),
            background_image_opacity: self.background_image_opacity,
        };

        let agent = AgentConfig {
            enable_agent_hooks: self.enable_agent_hooks,
            show_agent_usage: self.show_agent_usage,
            collapse_tool_calls: self.collapse_tool_calls,
        };

        let system = SystemConfig {
            restore_last_session_when_opening: self.restore_last_session_when_opening,
            manage_subprocess_job: self.manage_subprocess_job,
            warn_before_terminating_shell: self.warn_before_terminating_shell,
            confirm_before_closing_workspace: self.confirm_before_closing_workspace,
            prioritize_ui_threads: self.prioritize_ui_threads,
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

fn effective_background_opacity(transparency_enabled: bool, opacity: f64) -> f64 {
    if transparency_enabled { opacity } else { 1.0 }
}

fn effective_surface_background_opacity(window_opacity: f64, image_opacity: Option<f64>) -> f64 {
    window_opacity * (1.0 - image_opacity.unwrap_or(0.0))
}

pub(crate) fn surface_background_opacity(cx: &App) -> f32 {
    let settings = cx.global::<AppSettings>();

    effective_surface_background_opacity(
        effective_background_opacity(
            settings.window_transparency_enabled,
            settings.background_opacity,
        ),
        settings
            .background_image
            .as_ref()
            .map(|_| settings.background_image_opacity),
    ) as f32
}

fn effective_background_image_layer_opacity(window_opacity: f64, image_opacity: f64) -> f64 {
    let uncovered = 1.0 - effective_surface_background_opacity(window_opacity, Some(image_opacity));

    if uncovered > 0.0 {
        window_opacity * image_opacity / uncovered
    } else {
        0.0
    }
}

pub(crate) fn background_image_layer_opacity(cx: &App) -> f32 {
    let settings = cx.global::<AppSettings>();

    effective_background_image_layer_opacity(
        effective_background_opacity(
            settings.window_transparency_enabled,
            settings.background_opacity,
        ),
        settings.background_image_opacity,
    ) as f32
}

/// Apply the UI half of a terminal theme, falling back to the built-in dark
/// palette when the theme does not define `[colors.ui]` or contains invalid UI data.
pub(crate) fn apply_ui_theme(value: Option<&UiTheme>, cx: &mut App) {
    let configured = value.and_then(|value| {
        let mut config = TomlTable::new();

        config.insert("name".to_string(), TomlValue::String(value.name.clone()));
        config.insert(
            "mode".to_string(),
            TomlValue::String(
                match value.mode {
                    AppearanceTheme::Dark => "dark",
                    AppearanceTheme::Light => "light",
                }
                .to_string(),
            ),
        );

        let mut colors = value.colors.clone();

        // Size/behavior tokens live at the top level of `ThemeConfig`, but the
        // theme file format keeps everything under `[colors.ui]` — lift them
        // out so themes can set corner radii (they'd otherwise be silently
        // ignored inside the colors table).
        if let Some(colors) = colors.as_table_mut() {
            for key in ["radius", "radius.lg", "shadow"] {
                if let Some(v) = colors.remove(key) {
                    config.insert(key.to_string(), v);
                }
            }
        }

        config.insert("colors".to_string(), colors);

        TomlValue::Table(config)
            .try_into::<ComponentThemeConfig>()
            .map(Rc::new)
            .map_err(|err| warn!("failed to load UI theme: {err}"))
            .ok()
    });

    let theme = configured.unwrap_or_else(|| {
        ComponentThemeRegistry::global(cx)
            .default_dark_theme()
            .clone()
    });

    let mode = theme.mode;

    ComponentTheme::global_mut(cx).apply_config(&theme);
    ComponentTheme::change(mode, None, cx);
}

fn select_theme(name: String, cx: &mut App) {
    let theme = if name.is_empty() {
        Ok(Theme::default())
    } else {
        Config::load_named_theme(&name)
    };

    match theme {
        Ok(theme) => {
            set_active_colors(theme.colors.terminal);

            apply_ui_theme(theme.ui_theme().as_ref(), cx);

            cx.update_global(|settings: &mut AppSettings, _| settings.theme = name);

            apply_window_translucency(cx);

            cx.refresh_windows();
        }
        Err(err) => warn!("failed to select theme {name}: {err}"),
    }
}

fn load_theme_choices() -> Vec<(String, Theme)> {
    Config::load_themes()
}

fn reload_themes(cx: &mut App) {
    cx.global_mut::<AppSettings>().themes = load_theme_choices();

    let selected = cx.global::<AppSettings>().theme.clone();

    if selected.is_empty() {
        cx.refresh_windows();
    } else {
        select_theme(selected, cx);
    }
}

pub(crate) fn watch_themes(cx: &mut App) -> Option<Task<()>> {
    reload_themes(cx);

    let themes_dir = config_dir_path().join("themes");

    if let Err(err) = fs::create_dir_all(&themes_dir) {
        warn!("failed to create themes directory: {err}");
        return None;
    }

    let (tx, mut rx) = unbounded();

    let mut watcher = match recommended_watcher(move |event: NotifyResult<NotifyEvent>| {
        if event.is_ok() {
            let _ = tx.unbounded_send(());
        }
    }) {
        Ok(watcher) => watcher,
        Err(err) => {
            warn!("failed to watch themes directory: {err}");
            return None;
        }
    };

    if let Err(err) = watcher.watch(&themes_dir, RecursiveMode::NonRecursive) {
        warn!("failed to watch themes directory: {err}");
        return None;
    }

    Some(cx.spawn(async move |cx| {
        let _watcher = watcher;

        while rx.next().await.is_some() {
            let _ = cx.update(reload_themes);
        }
    }))
}

fn preview_color(color: ColorArray) -> Hsla {
    let channel = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u32;

    rgba(
        channel(color[0]) << 24
            | channel(color[1]) << 16
            | channel(color[2]) << 8
            | channel(color[3]),
    )
    .into()
}

fn theme_preview(colors: Colors) -> Div {
    let swatches = [
        colors.red,
        colors.yellow,
        colors.green,
        colors.cyan,
        colors.blue,
        colors.magenta,
    ];

    v_flex()
        .w_full()
        .h(px(72.0))
        .p_3()
        .gap_2()
        .rounded_md()
        .bg(preview_color(colors.background.0))
        .child(
            h_flex()
                .gap_2()
                .child(
                    div()
                        .text_color(preview_color(colors.foreground))
                        .child("ls"),
                )
                .child(div().text_color(preview_color(colors.blue)).child("dir"))
                .child(
                    div()
                        .text_color(preview_color(colors.red))
                        .child("executable"),
                )
                .child(
                    div()
                        .text_color(preview_color(colors.foreground))
                        .child("file"),
                ),
        )
        .child(h_flex().gap_1().children(swatches.into_iter().map(|color| {
            div()
                .w(px(18.0))
                .h(px(6.0))
                .rounded_sm()
                .bg(preview_color(color))
        })))
}

fn theme_list(cx: &mut App) -> Div {
    let selected = cx.global::<AppSettings>().theme.clone();
    let filter = cx.global::<AppSettings>().theme_filter.to_lowercase();

    let themes = cx
        .global::<AppSettings>()
        .themes
        .clone()
        .into_iter()
        .filter(|(name, theme)| {
            let display_name = if name.is_empty() { "Default" } else { name };
            filter.is_empty()
                || display_name.to_lowercase().contains(&filter)
                || theme.name.to_lowercase().contains(&filter)
        })
        .collect::<Vec<_>>();

    let border = cx.theme().border;
    let selected_border = cx.theme().primary;
    let selected_background = cx.theme().tokens.secondary;
    let hover_background = cx.theme().tokens.secondary_hover;

    v_flex()
        .w_full()
        .gap_2()
        .child(
            h_flex().justify_between().child("Themes").child(
                Button::new("theme-refresh")
                    .outline()
                    .label("Refresh")
                    .on_click(|_, _, cx: &mut App| reload_themes(cx)),
            ),
        )
        .when(themes.is_empty(), |this| {
            this.child(
                div()
                    .py_4()
                    .text_color(cx.theme().muted_foreground)
                    .child("No matching .toml themes found."),
            )
        })
        .children(
            themes
                .into_iter()
                .enumerate()
                .map(|(index, (name, theme))| {
                    let is_selected = name == selected;

                    let display_name = if theme.name.is_empty() {
                        if name.is_empty() { "Default" } else { &name }
                    } else {
                        &theme.name
                    }
                    .to_string();

                    div()
                        .id(("theme-card", index))
                        .w_full()
                        .p_3()
                        .rounded_lg()
                        .border_1()
                        .border_color(if is_selected { selected_border } else { border })
                        .when(is_selected, |this| this.bg(selected_background))
                        .hover(move |this| this.bg(hover_background))
                        .cursor_pointer()
                        .on_click(move |_, _, cx| select_theme(name.clone(), cx))
                        .child(theme_preview(theme.colors.terminal))
                        .child(
                            h_flex().mt_2().justify_between().child(display_name).when(
                                is_selected,
                                |this| {
                                    this.child(div().text_color(selected_border).child("Selected"))
                                },
                            ),
                        )
                }),
        )
}

/// Retint the component theme for the foreground surface opacity. A configured
/// image shows through by reducing this tint; without an image it remains the
/// effective window opacity. Reset first so repeated calls do not compound alpha.
pub(crate) fn apply_window_translucency(cx: &mut App) {
    let opacity = surface_background_opacity(cx);
    let theme = ComponentTheme::global_mut(cx);

    let palette = if theme.mode.is_dark() {
        theme.dark_theme.clone()
    } else {
        theme.light_theme.clone()
    };

    theme.apply_config(&palette);

    if opacity < 1.0 {
        theme.colors.sidebar = theme.colors.sidebar.opacity(opacity);

        // The shell paints this across the whole window as the chrome base
        // layer; it must dim with the rest of the chrome or translucency would
        // be defeated by an opaque backdrop.
        theme.colors.background = theme.colors.background.opacity(opacity);

        for token in [
            &mut theme.tokens.title_bar,
            &mut theme.tokens.tab_bar,
            &mut theme.tokens.tab_active,
        ] {
            let color = token.color.opacity(opacity);
            *token = ComponentThemeToken::new(color, color.into());
        }
    }
}

#[derive(Clone, Copy)]
enum OpacityTarget {
    Window,
    Image,
}

impl OpacityTarget {
    fn value(self, settings: &AppSettings) -> f64 {
        match self {
            Self::Window => settings.background_opacity,
            Self::Image => settings.background_image_opacity,
        }
    }

    fn min(self) -> f32 {
        match self {
            Self::Window => 0.2,
            Self::Image => 0.0,
        }
    }

    fn set(self, value: f64, settings: &mut AppSettings) {
        match self {
            Self::Window => settings.background_opacity = clamp_background_opacity(value),
            Self::Image => {
                settings.background_image_opacity = clamp_background_image_opacity(value)
            }
        }
    }
}

/// Both opacity fields share persistent slider entities because the settings
/// view and its field closures are rebuilt every render.
struct OpacitySliderState {
    window: Entity<SliderState>,
    image: Entity<SliderState>,
    _subscriptions: [Subscription; 2],
}

impl Global for OpacitySliderState {}

fn opacity_slider_field(target: OpacityTarget) -> SettingField<SharedString> {
    SettingField::render(move |options, window, cx| {
        if !cx.has_global::<OpacitySliderState>() {
            let make_slider = |target: OpacityTarget, cx: &mut App| {
                let value = target.value(cx.global::<AppSettings>()) as f32;
                let slider = cx.new(|_| {
                    SliderState::new()
                        .min(target.min())
                        .max(1.0)
                        .step(0.05)
                        .default_value(value)
                });

                let subscription = cx.subscribe(&slider, move |_, event: &SliderEvent, cx| {
                    let (SliderEvent::Change(value) | SliderEvent::Release(value)) = event;
                    target.set(value.end() as f64, cx.global_mut::<AppSettings>());
                });

                (slider, subscription)
            };

            let (window_slider, window_subscription) = make_slider(OpacityTarget::Window, cx);
            let (image_slider, image_subscription) = make_slider(OpacityTarget::Image, cx);

            cx.set_global(OpacitySliderState {
                window: window_slider,
                image: image_slider,
                _subscriptions: [window_subscription, image_subscription],
            });
        }

        let sliders = cx.global::<OpacitySliderState>();
        let slider = match target {
            OpacityTarget::Window => &sliders.window,
            OpacityTarget::Image => &sliders.image,
        }
        .clone();

        let current = target.value(cx.global::<AppSettings>()) as f32;

        if (slider.read(cx).value().end() - current).abs() > 0.001 {
            slider.update(cx, |state, cx| state.set_value(current, window, cx));
        }

        h_flex()
            // The setting row's field slot is auto-sized, so a percentage
            // width resolves to the content width (zero for the slider bar)
            // and the whole control collapses; horizontal layout needs a
            // fixed width, like NumberField's `w_32`.
            .map(|this| {
                if options.layout.is_horizontal() {
                    this.w_56()
                } else {
                    this.w_full()
                }
            })
            .gap_2()
            // The thumb (16px, centered on the track position) overhangs the
            // track by 8px at either end; pad so it stays inside the setting
            // row's overflow_hidden instead of being clipped at min/max.
            //
            // Thumb color: the dark theme leaves `slider.thumb` unset and its
            // `primary_foreground` fallback (neutral-900) vanishes against the
            // neutral-950 panel, so use `primary`, which contrasts with the
            // panel in both modes.
            .child(
                div().flex_1().px_2().child(
                    Slider::new(&slider)
                        .disabled(options.disabled)
                        .text_color(cx.theme().primary),
                ),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .child(SharedString::from(format!("{current:.2}"))),
            )
    })
}

fn background_opacity_field() -> SettingField<SharedString> {
    opacity_slider_field(OpacityTarget::Window)
}

fn background_image_opacity_field() -> SettingField<SharedString> {
    opacity_slider_field(OpacityTarget::Image)
}

fn background_image_field() -> SettingField<SharedString> {
    SettingField::render(|options, _window, cx| {
        let path = cx.global::<AppSettings>().background_image.clone();
        let label = SharedString::from(path.clone().unwrap_or_else(|| "None".to_string()));

        h_flex()
            .map(|this| {
                if options.layout.is_horizontal() {
                    this.w_64()
                } else {
                    this.w_full()
                }
            })
            .gap_2()
            .child(div().flex_1().min_w_0().truncate().child(label))
            .child(
                Button::new("background-image-browse")
                    .outline()
                    .label("Browse")
                    .disabled(options.disabled)
                    .on_click(|_, window, cx| {
                        let rx = cx.prompt_for_paths(PathPromptOptions {
                            files: true,
                            directories: false,
                            multiple: false,
                            prompt: Some("Select background image".into()),
                            file_types: vec![FileDialogFilter {
                                name: "Images".into(),
                                extensions: ["png", "jpg", "jpeg", "webp", "bmp"]
                                    .into_iter()
                                    .map(Into::into)
                                    .collect(),
                            }],
                        });
                        window
                            .spawn(cx, async move |cx| {
                                if let Ok(Ok(Some(paths))) = rx.await
                                    && let Some(path) = paths.first()
                                {
                                    let path = path.display().to_string();
                                    let _ = cx.update_global(|settings: &mut AppSettings, _, _| {
                                        settings.background_image = Some(path);
                                    });
                                }
                            })
                            .detach();
                    }),
            )
            .children(path.is_some().then(|| {
                Button::new("background-image-clear")
                    .outline()
                    .label("Clear")
                    .disabled(options.disabled)
                    .on_click(|_, _, cx: &mut App| {
                        cx.global_mut::<AppSettings>().background_image = None;
                    })
            }))
    })
}

fn window_background_appearance_for(transparency_enabled: bool) -> WindowBackgroundAppearance {
    if transparency_enabled {
        WindowBackgroundAppearance::Blurred
    } else {
        WindowBackgroundAppearance::Opaque
    }
}

/// Select acrylic composition only while the alpha-capable target is enabled.
pub(crate) fn window_background_appearance(cx: &App) -> WindowBackgroundAppearance {
    window_background_appearance_for(cx.global::<AppSettings>().window_transparency_enabled)
}

fn agent_hook_item(
    name: &'static str,
    detection_path: Option<path::PathBuf>,
    hooks_path: Option<path::PathBuf>,
    status: fn(&path::Path) -> HookInstallStatus,
    install: fn(&path::Path) -> io::Result<()>,
    uninstall: fn(&path::Path) -> io::Result<()>,
) -> SettingItem {
    let detected = detection_path.as_ref().is_some_and(|path| path.is_file());
    let status_path = hooks_path.clone();
    let action_path = hooks_path;

    SettingItem::new(
        name,
        SettingField::checkbox(
            // Settings renders only the active page, so a disk-backed getter
            // refreshes Hook state whenever the user enters the Agent page.
            move |_| {
                status_path
                    .as_deref()
                    .is_some_and(|path| status(path) == HookInstallStatus::Installed)
            },
            move |enabled, cx| {
                let Some(path) = action_path.as_deref() else {
                    return;
                };

                let result = if enabled {
                    install(path)
                } else {
                    uninstall(path)
                };

                if let Err(error) = result {
                    warn!("failed to update {name} hooks: {error}");
                }

                cx.refresh_windows();
            },
        ),
    )
    .disabled(!detected)
}

fn agent_page() -> SettingPage {
    SettingPage::new("Agent")
        .default_open(true)
        .description("Configure Agent event handling and per-Agent Hook installation.")
        .group(
            SettingGroup::new()
                .title("General")
                .item(
                    SettingItem::new(
                        "Enable Agent Hooks",
                        SettingField::switch(
                            |cx| cx.global::<AppSettings>().enable_agent_hooks,
                            |value, cx| {
                                cx.global_mut::<AppSettings>().enable_agent_hooks = value;
                            },
                        ),
                    )
                    .description(
                        "Process new lifecycle events from installed Agent Hooks. This does not change their installation state.",
                    ),
                )
                .item(
                    SettingItem::new(
                        "Show Agent Usage",
                        SettingField::switch(
                            |cx| cx.global::<AppSettings>().show_agent_usage,
                            |value, cx| {
                                cx.global_mut::<AppSettings>().show_agent_usage = value;
                            },
                        ),
                    )
                    .description("Show Agent account usage in the workspace sidebar."),
                )
                .item(
                    SettingItem::new(
                        "Collapse Tool Call details by default",
                        SettingField::switch(
                            |cx| cx.global::<AppSettings>().collapse_tool_calls,
                            |value, cx| {
                                cx.global_mut::<AppSettings>().collapse_tool_calls = value;
                            },
                        ),
                    )
                    .description(
                        "In agent tabs, show only the newest of consecutive tool calls; older \
                         ones sit behind a \"+N previous tool calls\" toggle.",
                    ),
                ),
        )
        .group(
            SettingGroup::new()
                .title("Installed Agents")
                .item(agent_hook_item(
                    "Claude Code",
                    claude_hook::settings_path(),
                    claude_hook::settings_path(),
                    claude_hook::hooks_status,
                    claude_hook::install_hooks,
                    claude_hook::uninstall_hooks,
                ))
                .item(agent_hook_item(
                    "Codex",
                    codex_hook::config_path(),
                    codex_hook::hooks_path(),
                    codex_hook::hooks_status,
                    codex_hook::install_hooks,
                    codex_hook::uninstall_hooks,
                )),
        )
}

fn remote_session_page() -> SettingPage {
    SettingPage::new("Remote Session")
        .default_open(true)
        .description(
            "Reach this machine's terminal sessions from other computers through a relay. \
             Traffic is end-to-end encrypted; the relay only ever sees ciphertext.",
        )
        .group(
            SettingGroup::new()
                .title("Host Service")
                .item(
                    SettingItem::new(
                        "Enable Host Service",
                        SettingField::switch(
                            |cx| cx.global::<AppSettings>().remote_host_enabled,
                            |value, cx| {
                                cx.global_mut::<AppSettings>().remote_host_enabled = value;
                                reconcile_remote_host(cx);
                            },
                        ),
                    )
                    .description(
                        "Register with the relay so paired devices can attach to sessions on \
                         this machine. Sessions keep running while no client is connected.",
                    ),
                )
                .item(
                    SettingItem::new(
                        "Relay URL",
                        SettingField::input(
                            |cx| cx.global::<AppSettings>().remote_relay_url.clone(),
                            |value, cx| {
                                cx.global_mut::<AppSettings>().remote_relay_url = value;
                            },
                        ),
                    )
                    .description(
                        "WebSocket endpoint, e.g. wss://relay.example.com/ws. Applied when you \
                         toggle the service or close settings.",
                    ),
                )
                .item(
                    SettingItem::new(
                        "Access Token",
                        SettingField::input(
                            |cx| cx.global::<AppSettings>().remote_access_token.clone(),
                            |value, cx| {
                                cx.global_mut::<AppSettings>().remote_access_token = value;
                            },
                        ),
                    )
                    .description("Shared secret the relay requires to register this host."),
                ),
        )
        .group(
            SettingGroup::new()
                .title("Pairing & Devices")
                .item(SettingItem::render(|_, _, cx| remote_host_status(cx))),
        )
        .group(
            SettingGroup::new()
                .title("Connect to a Host")
                .description(
                    "Pair with another machine's host service using the code it shows, then \
                     open remote tabs with Ctrl+Shift+R.",
                )
                .item(
                    SettingItem::new(
                        "Pairing Code",
                        SettingField::input(
                            |cx| cx.global::<AppSettings>().remote_pairing_input.clone(),
                            |value, cx| {
                                cx.global_mut::<AppSettings>().remote_pairing_input = value;
                            },
                        ),
                    )
                    .description("Paste the code from the host machine, then click Pair."),
                )
                .item(SettingItem::render(|_, _, cx| remote_client_status(cx))),
        )
}

/// Start/stop/restart the background host service to match the live settings.
/// Called on discrete events (enable toggle, dialog close), never per keystroke.
#[cfg(windows)]
pub(crate) fn reconcile_remote_host(cx: &App) {
    let settings = cx.global::<AppSettings>();
    remote::reconcile(&RemoteSessionConfig {
        host_enabled: settings.remote_host_enabled,
        relay_url: settings.remote_relay_url.to_string(),
        access_token: settings.remote_access_token.to_string(),
    });
}

#[cfg(not(windows))]
pub(crate) fn reconcile_remote_host(_cx: &App) {}

#[cfg(windows)]
fn remote_host_status(cx: &mut App) -> Div {
    use crate::remote;

    let muted = cx.theme().muted_foreground;
    let border = cx.theme().border;
    let surface = cx.theme().tokens.secondary;

    if !remote::is_running() {
        return v_flex().child(
            div()
                .py_2()
                .text_color(muted)
                .child("Enable the host service (with a relay URL and token) to pair devices."),
        );
    }

    let host_id = remote::host_id().unwrap_or_default();
    let pairing = cx.global::<AppSettings>().remote_pairing_code.clone();
    let devices = remote::list_devices();

    v_flex()
        .w_full()
        .gap_3()
        .child(
            h_flex().gap_2().child("Host ID").child(
                div()
                    .font_family("monospace")
                    .text_color(muted)
                    .child(host_id),
            ),
        )
        .child(
            h_flex().justify_between().child("Pair a new device").child(
                Button::new("remote-generate-pairing")
                    .outline()
                    .label("Generate Pairing Code")
                    .on_click(|_, _, cx: &mut App| {
                        if let Some(code) = remote::begin_pairing() {
                            cx.global_mut::<AppSettings>().remote_pairing_code =
                                Some(code.encode());
                        }
                    }),
            ),
        )
        .when_some(pairing, |this, code| {
            this.child(
                v_flex()
                    .gap_2()
                    .p_3()
                    .rounded_lg()
                    .border_1()
                    .border_color(border)
                    .bg(surface)
                    .child(
                        div()
                            .text_color(muted)
                            .child("Enter this code on the other computer within 5 minutes:"),
                    )
                    .child(div().font_family("monospace").child(code.clone()))
                    .child(
                        Button::new("remote-copy-pairing")
                            .outline()
                            .label("Copy")
                            .on_click(move |_, _, cx: &mut App| {
                                cx.write_to_clipboard(ClipboardItem::new_string(code.clone()));
                            }),
                    ),
            )
        })
        .child(div().mt_2().text_color(muted).child("Authorized Devices"))
        .when(devices.is_empty(), |this| {
            this.child(
                div()
                    .py_2()
                    .text_color(muted)
                    .child("No devices paired yet."),
            )
        })
        .children(devices.into_iter().enumerate().map(|(index, device)| {
            let key = device.public_key.clone();
            h_flex()
                .w_full()
                .py_2()
                .justify_between()
                .border_b_1()
                .border_color(border)
                .child(device.name)
                .child(
                    Button::new(("remote-revoke", index))
                        .outline()
                        .label("Revoke")
                        .on_click(move |_, _, cx: &mut App| {
                            remote::revoke_device(&key);
                            cx.refresh_windows();
                        }),
                )
        }))
}

#[cfg(not(windows))]
fn remote_host_status(_cx: &mut App) -> Div {
    v_flex().child(div().child("Remote sessions are only available on Windows."))
}

#[cfg(windows)]
fn remote_client_status(cx: &mut App) -> Div {
    use crate::remote;

    let muted = cx.theme().muted_foreground;
    let border = cx.theme().border;
    let status = cx.global::<AppSettings>().remote_client_status.clone();
    let hosts = remote::known_hosts();

    v_flex()
        .w_full()
        .gap_3()
        .child(
            h_flex()
                .justify_between()
                .child("Pair with this code")
                .child(Button::new("remote-pair").outline().label("Pair").on_click(
                    |_, _, cx: &mut App| {
                        let code = cx.global::<AppSettings>().remote_pairing_input.to_string();
                        if code.trim().is_empty() {
                            cx.global_mut::<AppSettings>().remote_client_status =
                                Some("Enter a pairing code first.".to_owned());
                            return;
                        }
                        cx.global_mut::<AppSettings>().remote_client_status =
                            Some("Pairing…".to_owned());
                        // Pairing is a network round trip: running it inline
                        // would freeze the window until the relay answers or
                        // the attempt times out.
                        cx.spawn(async move |cx| {
                            let paired = cx
                                .background_executor()
                                .spawn(async move { remote::pair_with_code(&code, "remote host") })
                                .await;
                            cx.update_global(|settings: &mut AppSettings, _| {
                                let message = match paired {
                                    Ok(host) => {
                                        settings.remote_pairing_input = SharedString::default();
                                        format!("Paired with {} ({}).", host.name, host.host_id)
                                    }
                                    Err(e) => format!("Pairing failed: {e}"),
                                };
                                settings.remote_client_status = Some(message);
                            })
                        })
                        .detach();
                    },
                )),
        )
        .when_some(status, |this, message| {
            this.child(div().text_color(muted).child(message))
        })
        .child(div().mt_2().text_color(muted).child("Paired Hosts"))
        .when(hosts.is_empty(), |this| {
            this.child(div().py_2().text_color(muted).child("No hosts paired yet."))
        })
        .children(hosts.into_iter().enumerate().map(|(index, host)| {
            let host_id = host.host_id.clone();
            h_flex()
                .w_full()
                .py_2()
                .justify_between()
                .border_b_1()
                .border_color(border)
                .child(
                    v_flex().child(host.name.clone()).child(
                        div()
                            .font_family("monospace")
                            .text_color(muted)
                            .child(host.host_id.clone()),
                    ),
                )
                .child(
                    Button::new(("remote-forget", index))
                        .outline()
                        .label("Forget")
                        .on_click(move |_, _, cx: &mut App| {
                            remote::forget_host(&host_id);
                            cx.refresh_windows();
                        }),
                )
        }))
}

#[cfg(not(windows))]
fn remote_client_status(_cx: &mut App) -> Div {
    v_flex()
}

pub fn settings_view(cx: &App) -> Settings {
    let profiles = cx.global::<AppSettings>().profiles.clone();
    let agent_profiles = cx.global::<AppSettings>().agent_profiles.clone();
    let transparency_enabled = cx.global::<AppSettings>().window_transparency_enabled;
    let background_image_enabled = cx.global::<AppSettings>().background_image.is_some();
    let shell_integration_mismatched = shell_integration_dll_mismatched();

    let sidebar_style = StyleRefinement::default()
        .bg(cx.theme().sidebar)
        .border_t_1()
        .border_b_1()
        .border_l_1()
        .border_color(cx.theme().sidebar_border)
        .rounded(cx.theme().radius_lg)
        .overflow_hidden();

    Settings::new("app-settings")
        .sidebar_width(px(240.0))
        .sidebar_style(&sidebar_style)
        // Each subcategory is its own page; the alternative scrolls the
        // whole category top to bottom.
        .single_group_pages(true)
        .page(
            SettingPage::new("Terminal").default_open(true).group(
                SettingGroup::new()
                    .title("Input")
                    .item(
                        SettingItem::new(
                            "Input Style",
                            SettingField::dropdown(
                                vec![
                                    (
                                        InputStyle::Waterfall.as_str().into(),
                                        input_style_label(InputStyle::Waterfall).into(),
                                    ),
                                    (
                                        InputStyle::FixedBottom.as_str().into(),
                                        input_style_label(InputStyle::FixedBottom).into(),
                                    ),
                                ],
                                |cx| cx.global::<AppSettings>().input_style.as_str().into(),
                                |value, cx| {
                                    cx.global_mut::<AppSettings>().input_style =
                                        input_style_from_value(&value);
                                },
                            )
                            .default_value(SharedString::from(InputStyle::Waterfall.as_str())),
                        )
                        .description("How the prompt input is presented."),
                    )
                    .item(
                        SettingItem::new(
                            "Cursor Shape",
                            SettingField::dropdown(
                                vec![
                                    ("block".into(), "Block".into()),
                                    ("line".into(), "Line".into()),
                                    ("underline".into(), "Underline".into()),
                                ],
                                |cx| cx.global::<AppSettings>().cursor_shape.as_str().into(),
                                |value, cx| {
                                    cx.global_mut::<AppSettings>().cursor_shape =
                                        cursor_shape_from_value(&value);
                                },
                            )
                            .default_value(SharedString::from("block")),
                        )
                        .description("Default cursor shape used by newly opened terminals."),
                    )
                    .item(
                        SettingItem::new(
                            "Command Blocks",
                            SettingField::switch(
                                |cx| cx.global::<AppSettings>().command_blocks,
                                |value, cx| {
                                    cx.global_mut::<AppSettings>().command_blocks = value;
                                },
                            ),
                        )
                        .description(
                            "Group each command's output into a block with a separator, \
                             exit status, and duration. Off: outputs run together like a \
                             classic terminal.",
                        ),
                    ),
            ),
        )
        .page(
            SettingPage::new("Appearance")
                .default_open(true)
                .group(
                    SettingGroup::new()
                        .title("Theme")
                        .description(
                            "Themes are loaded from the themes directory and applied immediately.",
                        )
                        .item(
                            SettingItem::new(
                                "Search",
                                SettingField::input(
                                    |cx| {
                                        cx.global::<AppSettings>().theme_filter.clone().into()
                                    },
                                    |value, cx| {
                                        cx.global_mut::<AppSettings>().theme_filter =
                                            value.to_string();
                                    },
                                ),
                            )
                            .description("Filter themes by file name or UI theme name."),
                        )
                        .item(SettingItem::render(|_, _, cx| theme_list(cx)).keywords([
                            "theme",
                            "colors",
                            "palette",
                        ])),
                )
                .group(
                    SettingGroup::new()
                        .title("Window")
                        .item(
                            SettingItem::new(
                                "Enable Window Transparency",
                                SettingField::switch(
                                    |cx| {
                                        cx.global::<AppSettings>().window_transparency_enabled
                                    },
                                    |value, cx| {
                                        cx.global_mut::<AppSettings>()
                                            .window_transparency_enabled = value;
                                    },
                                ),
                            )
                            .description(
                                "Use an acrylic backdrop and preserve window alpha for live transparency.",
                            ),
                        )
                        .item(
                            SettingItem::new("Background Opacity", background_opacity_field())
                                .description(
                                    "Whole-window opacity while window transparency is enabled.",
                                )
                                .disabled(!transparency_enabled),
                        )
                        .item(
                            SettingItem::new("Background Image", background_image_field())
                                .description("Local image stretched to cover the whole window."),
                        )
                        .item(
                            SettingItem::new(
                                "Background Image Opacity",
                                background_image_opacity_field(),
                            )
                            .description("How strongly the image shows through window surfaces.")
                            .disabled(!background_image_enabled),
                        ),
                )
                .group(
                    SettingGroup::new()
                        .title("Font")
                        .item(
                            SettingItem::new(
                                "UI Font",
                                ui::font_picker::font_family_field(
                                    ui::font_picker::FontTarget::Ui,
                                ),
                            )
                            .description(
                                "Font for the app chrome (titlebar, sidebar, tabs, dialogs).",
                            ),
                        )
                        .item(
                            SettingItem::new(
                                "Terminal Font",
                                ui::font_picker::font_family_field(
                                    ui::font_picker::FontTarget::Terminal,
                                ),
                            )
                            .description("Font used by the terminal view."),
                        )
                        .item(
                            SettingItem::new(
                                "Terminal Font Size",
                                SettingField::number_input(
                                    NumberFieldOptions {
                                        min: 6.0,
                                        max: 72.0,
                                        step: 0.1,
                                    },
                                    |cx| cx.global::<AppSettings>().terminal_font_size,
                                    |value, cx| {
                                        cx.global_mut::<AppSettings>().terminal_font_size = value;
                                    },
                                ),
                            )
                            .description("Font size in pixels."),
                        )
                        .item(
                            SettingItem::new(
                                "Terminal Line Height",
                                SettingField::number_input(
                                    NumberFieldOptions {
                                        min: 0.8,
                                        max: 3.0,
                                        step: 0.1,
                                    },
                                    |cx| cx.global::<AppSettings>().terminal_line_height,
                                    |value, cx| {
                                        cx.global_mut::<AppSettings>().terminal_line_height = value;
                                    },
                                ),
                            )
                            .description("Line height as a multiplier on font size."),
                        )
                        .item(
                            SettingItem::new(
                                "Agent Font",
                                ui::font_picker::font_family_field(
                                    ui::font_picker::FontTarget::Agent,
                                ),
                            )
                            .description("Font used by agent (chat) tabs."),
                        )
                        .item(
                            SettingItem::new(
                                "Agent Font Size",
                                SettingField::number_input(
                                    NumberFieldOptions {
                                        min: 6.0,
                                        max: 72.0,
                                        step: 0.1,
                                    },
                                    |cx| cx.global::<AppSettings>().agent_font_size,
                                    |value, cx| {
                                        cx.global_mut::<AppSettings>().agent_font_size = value;
                                    },
                                ),
                            )
                            .description("Font size in pixels."),
                        )
                        .item(
                            SettingItem::new(
                                "Show monospace fonts only",
                                SettingField::switch(
                                    |cx| cx.global::<AppSettings>().monospace_only,
                                    |value, cx| {
                                        cx.global_mut::<AppSettings>().monospace_only = value;
                                    },
                                ),
                            )
                            .description("Filter the font list to fixed-width fonts."),
                        ),
                )
                .group(
                    SettingGroup::new().title("Tab Bar").item(
                        SettingItem::new(
                            "Tab Width",
                            SettingField::number_input(
                                NumberFieldOptions {
                                    min: DEFAULT_TAB_WIDTH,
                                    max: MAX_TAB_WIDTH,
                                    step: 1.0,
                                },
                                |cx| cx.global::<AppSettings>().tab_width,
                                |value, cx| {
                                    cx.global_mut::<AppSettings>().tab_width =
                                        clamp_tab_width(value);
                                },
                            ),
                        )
                        .description("Fixed tab width in pixels; long titles are clipped."),
                    ),
                )
                .group(
                    SettingGroup::new()
                        .title("Title Bar")
                        .item(
                            SettingItem::new(
                                "Show daily token usage",
                                SettingField::switch(
                                    |cx| cx.global::<AppSettings>().show_daily_token_usage,
                                    |value, cx| {
                                        cx.global_mut::<AppSettings>().show_daily_token_usage =
                                            value;
                                    },
                                ),
                            )
                            .description(
                                "Show today's ccusage token totals in the titlebar, \
                         refreshed every 60 seconds (click to refresh now).",
                            ),
                        )
                        .item(
                            SettingItem::new(
                                "Show Git Status on Title Bar",
                                SettingField::switch(
                                    |cx| cx.global::<AppSettings>().show_git_status_on_title_bar,
                                    |value, cx| {
                                        cx.global_mut::<AppSettings>()
                                            .show_git_status_on_title_bar = value;
                                    },
                                ),
                            )
                            .description(
                                "Show the active repository's +added -removed line \
                         counts in the titlebar.",
                            ),
                        )
                        .item(
                            SettingItem::new(
                                "Git Status Refresh Interval",
                                SettingField::dropdown(
                                    vec![
                                        ("10".into(), "10s".into()),
                                        ("15".into(), "15s".into()),
                                        ("30".into(), "30s".into()),
                                        ("60".into(), "60s".into()),
                                    ],
                                    |cx| {
                                        cx.global::<AppSettings>()
                                            .git_status_refresh_interval
                                            .to_string()
                                            .into()
                                    },
                                    |value, cx| {
                                        cx.global_mut::<AppSettings>()
                                            .git_status_refresh_interval =
                                            clamp_git_interval(value.parse().unwrap_or(30));
                                    },
                                )
                                .default_value(SharedString::from("30")),
                            )
                            .description("How often the git status is re-read."),
                        ),
                ),
        )
        .page(profiles_page(&profiles, &agent_profiles))
        .page(agent_page())
        .page(
            SettingPage::new("System")
                .default_open(true)
                .group(
                    SettingGroup::new().title("Session").item(
                        SettingItem::new(
                            "Restore last session when opening",
                            SettingField::switch(
                                |cx| cx.global::<AppSettings>().restore_last_session_when_opening,
                                |value, cx| {
                                    cx.global_mut::<AppSettings>()
                                        .restore_last_session_when_opening = value;
                                },
                            ),
                        )
                        .description("Reopen saved workspaces and tabs on startup."),
                    ),
                )
                .group(
                    SettingGroup::new().title("Workspace").item(
                        SettingItem::new(
                            "Confirm before closing workspace",
                            SettingField::switch(
                                |cx| cx.global::<AppSettings>().confirm_before_closing_workspace,
                                |value, cx| {
                                    cx.global_mut::<AppSettings>()
                                        .confirm_before_closing_workspace = value;
                                },
                            ),
                        )
                        .description("Ask for confirmation when closing a workspace."),
                    ),
                )
                .group(
                    SettingGroup::new()
                        .title("Process")
                        .item(
                            SettingItem::new(
                                "Manage subprocess by Windows Job API",
                                SettingField::switch(
                                    |cx| cx.global::<AppSettings>().manage_subprocess_job,
                                    |value, cx| {
                                        cx.global_mut::<AppSettings>().manage_subprocess_job =
                                            value;
                                    },
                                ),
                            )
                            .description(
                                "Closing a tab kills the shell's entire process tree. \
                         Applies to newly opened tabs.",
                            ),
                        )
                        .item(
                            SettingItem::new(
                                "Warn before terminating shell",
                                SettingField::dropdown(
                                    vec![
                                        ("disabled".into(), "Disabled".into()),
                                        (
                                            "when-child-processes-running".into(),
                                            "When child processes running".into(),
                                        ),
                                        ("always".into(), "Always".into()),
                                    ],
                                    |cx| {
                                        cx.global::<AppSettings>()
                                            .warn_before_terminating_shell
                                            .as_str()
                                            .into()
                                    },
                                    |value, cx| {
                                        cx.global_mut::<AppSettings>()
                                            .warn_before_terminating_shell =
                                            WarnBeforeTerminatingShell::from_value(&value);
                                    },
                                )
                                .default_value(SharedString::from(
                                    WarnBeforeTerminatingShell::WhenChildProcessesRunning.as_str(),
                                )),
                            )
                            .description(
                                "Choose when closing a shell asks for confirmation. Detecting \
                         child processes requires Job management.",
                            ),
                        ),
                )
                .group(
                    SettingGroup::new()
                        .title("Windows")
                        .item(
                            SettingItem::new(
                                if shell_integration_mismatched {
                                    "Enable Windows Context Menu  ⚠"
                                } else {
                                    "Enable Windows Context Menu"
                                },
                                SettingField::switch(
                                    |_| is_shell_integration_registered(),
                                    |value, _| {
                                        let result = if value {
                                            register_shell_integration()
                                        } else {
                                            unregister_shell_integration()
                                        };

                                        if let Err(err) = result {
                                            warn!(
                                                "failed to toggle Windows context menu: {err:#}"
                                            );
                                        }
                                    },
                                ),
                            )
                            .description(if shell_integration_mismatched {
                                "The registered shell extension does not point to the DLL beside the current NiumaTerm executable."
                            } else {
                                "Add NiumaTerm actions to File Explorer directory menus."
                            }),
                        )
                        .item(
                            SettingItem::new(
                                "Enable System Notification",
                                SettingField::switch(
                                    |_| system_notification_enabled(),
                                    |value, _| {
                                        if let Err(err) =
                                            set_system_notification_enabled(value)
                                        {
                                            warn!(
                                                "failed to toggle system notifications: {err:#}"
                                            );
                                        }
                                    },
                                ),
                            )
                            .description(
                                "Show Windows notifications for terminal and agent events.",
                            ),
                        ),
                )
                .group(
                    SettingGroup::new().title("Performance").item(
                        SettingItem::new(
                            "Prioritize UI threads",
                            SettingField::switch(
                                |cx| cx.global::<AppSettings>().prioritize_ui_threads,
                                |value, cx| {
                                    cx.global_mut::<AppSettings>().prioritize_ui_threads = value;

                                    cx.global::<PlatformHandle>()
                                        .0
                                        .set_ui_thread_priority(value);
                                },
                            ),
                        )
                        .description("Raise the main and render thread priority to AboveNormal."),
                    ),
                ),
        )
        .page(remote_session_page())
}

/// The Profiles page: exactly two groups — Terminal Profile and Agent
/// Profile — so the sidebar shows two stable entries. Profile cards render
/// inside each group instead of as their own groups, which would otherwise
/// add one sidebar entry per profile under `single_group_pages`.
fn profiles_page(profiles: &[Profile], agent_profiles: &[AgentProfile]) -> SettingPage {
    SettingPage::new("Profiles")
        .default_open(true)
        .group(terminal_profiles_group(profiles))
        .group(agent_profiles_group(agent_profiles))
}

/// One labeled row inside a profile card: title and muted description on the
/// left, the control on the right (mirrors `SettingItem`'s horizontal
/// layout so cards read like regular setting rows).
fn card_row(
    title: impl Into<SharedString>,
    description: impl Into<SharedString>,
    control: impl gpui::IntoElement,
    cx: &App,
) -> Div {
    h_flex()
        .w_full()
        .justify_between()
        .items_start()
        .gap_3()
        .child(
            v_flex()
                .flex_1()
                .max_w_3_5()
                .gap_1()
                .child(Label::new(title.into()).text_sm())
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(description.into()),
                ),
        )
        .child(control.into_any_element())
}

fn terminal_profiles_group(profiles: &[Profile]) -> SettingGroup {
    // Selector options come from the current names; the settings view is
    // rebuilt per render, so renames refresh the list immediately.
    let options: Vec<(SharedString, SharedString)> = profiles
        .iter()
        .enumerate()
        .map(|(ix, p)| {
            let label = if p.name.is_empty() {
                format!("Profile {}", ix + 1)
            } else {
                p.name.clone()
            };

            (
                SharedString::from(p.name.clone()),
                SharedString::from(label),
            )
        })
        .collect();

    let mut group = SettingGroup::new()
        .title("Terminal Profile")
        .description("Shell profiles available to terminals.")
        .item(
            SettingItem::new(
                "Default Profile",
                SettingField::dropdown(
                    options,
                    |cx| cx.global::<AppSettings>().default_profile.clone().into(),
                    |value, cx| {
                        cx.global_mut::<AppSettings>().default_profile = value.to_string();
                    },
                ),
            )
            .description("Profile used by new terminals."),
        )
        .item(
            SettingItem::new(
                "Add Profile",
                SettingField::render(|_, _, _| {
                    Button::new("profile-add").outline().label("Add").on_click(
                        |_, _, cx: &mut App| {
                            cx.global_mut::<AppSettings>().add_profile();
                        },
                    )
                }),
            )
            .description("Create a new profile."),
        );

    let count = profiles.len();
    for ix in 0..count {
        group = group.item(terminal_profile_card(ix, count));
    }
    group
}

fn terminal_profile_card(ix: usize, count: usize) -> SettingItem {
    SettingItem::render(move |options, window, cx| {
        // get(ix): the render closure outlives profile removal, so a stale
        // index must read as empty, not panic.
        let profile = cx
            .global::<AppSettings>()
            .profiles
            .get(ix)
            .cloned()
            .unwrap_or_default();

        let title = if profile.name.is_empty() {
            format!("Profile {}", ix + 1)
        } else {
            profile.name.clone()
        };

        let disabled = options.disabled;
        let size = options.size;

        let name_input = card_text_input(
            format!("terminal-profile-name-{ix}"),
            profile.name.clone().into(),
            false,
            move |value, cx| cx.global_mut::<AppSettings>().rename_profile(ix, value),
            window,
            cx,
        );

        let shell_input = card_text_input(
            format!("terminal-profile-shell-{ix}"),
            profile.shell.clone().into(),
            false,
            move |value, cx| {
                if let Some(profile) = cx.global_mut::<AppSettings>().profiles.get_mut(ix) {
                    profile.shell = value;
                }
            },
            window,
            cx,
        );

        let args_input = card_text_input(
            format!("terminal-profile-args-{ix}"),
            profile.args.clone().into(),
            false,
            move |value, cx| {
                if let Some(profile) = cx.global_mut::<AppSettings>().profiles.get_mut(ix) {
                    profile.args = value;
                }
            },
            window,
            cx,
        );

        let browse_input = shell_input.clone();
        let shell_control = v_flex()
            .gap_2()
            .w_64()
            .child(
                Input::new(&shell_input)
                    .disabled(disabled)
                    .with_size(size)
                    .w_full(),
            )
            .child(
                h_flex().w_full().justify_end().child(
                    Button::new(("profile-shell-browse", ix))
                        .outline()
                        .label("Browse")
                        .disabled(disabled)
                        .w(relative(1. / 3.))
                        .on_click(move |_, window, cx| {
                            let rx = cx.prompt_for_paths(PathPromptOptions {
                                files: true,
                                directories: false,
                                multiple: false,
                                prompt: Some("Select shell executable".into()),
                                file_types: vec![FileDialogFilter {
                                    name: "Executables".into(),
                                    extensions: vec!["exe".into()],
                                }],
                            });

                            let input = browse_input.clone();

                            window
                                .spawn(cx, async move |cx| {
                                    if let Ok(Ok(Some(paths))) = rx.await
                                        && let Some(path) = paths.first()
                                    {
                                        let value = path.display().to_string();

                                        let _ = input.update_in(cx, |input, window, cx| {
                                            input.set_value(value, window, cx);
                                        });
                                    }
                                })
                                .detach();
                        }),
                ),
            );

        let remove_button = Button::new(("profile-remove", ix))
            .danger()
            .label("Remove")
            .disabled(disabled || count <= 1)
            .on_click(move |_, window, cx: &mut App| {
                let name = cx
                    .global::<AppSettings>()
                    .profiles
                    .get(ix)
                    .map(|profile| profile.name.clone())
                    .unwrap_or_default();
                let subject = if name.is_empty() {
                    "this profile".to_string()
                } else {
                    format!("profile \"{name}\"")
                };

                window.open_alert_dialog(cx, move |alert, _, _| {
                    alert
                        .confirm()
                        .title("Remove Profile")
                        .description(format!("Remove {subject}? This cannot be undone."))
                        .on_ok(move |_, _, cx| {
                            cx.global_mut::<AppSettings>().remove_profile(ix);
                            true
                        })
                });
            });

        GroupBox::new().outline().title(title).child(
            v_flex()
                .w_full()
                .gap_4()
                .child(card_row(
                    "Name",
                    "Display name; the card title and default selector follow it.",
                    Input::new(&name_input)
                        .disabled(disabled)
                        .with_size(size)
                        .w_64(),
                    cx,
                ))
                .child(card_row(
                    "Shell Path",
                    "Path to the shell executable.",
                    shell_control,
                    cx,
                ))
                .child(card_row(
                    "Arguments",
                    "Command-line arguments, space-separated.",
                    Input::new(&args_input)
                        .disabled(disabled)
                        .with_size(size)
                        .w_64(),
                    cx,
                ))
                .child(card_row(
                    "Remove Profile",
                    if count <= 1 {
                        "The last profile cannot be removed."
                    } else {
                        "Removing the default falls back to the first profile."
                    },
                    remove_button,
                    cx,
                )),
        )
    })
}

fn agent_profiles_group(agent_profiles: &[AgentProfile]) -> SettingGroup {
    let options: Vec<(SharedString, SharedString)> = agent_profiles
        .iter()
        .enumerate()
        .map(|(ix, p)| {
            let label = if p.name.is_empty() {
                format!("Agent Profile {}", ix + 1)
            } else {
                p.name.clone()
            };

            (
                SharedString::from(p.name.clone()),
                SharedString::from(label),
            )
        })
        .collect();

    let mut group = SettingGroup::new()
        .title("Agent Profile")
        .description("Launch profiles for agent tabs (Claude Code and Codex).")
        .item(
            SettingItem::new(
                "Default Profile",
                SettingField::dropdown(
                    options,
                    |cx| {
                        cx.global::<AppSettings>()
                            .default_agent_profile
                            .clone()
                            .into()
                    },
                    |value, cx| {
                        cx.global_mut::<AppSettings>().default_agent_profile = value.to_string();
                    },
                ),
            )
            .description("Profile used by new agent tabs."),
        )
        .item(
            SettingItem::new(
                "Add Profile",
                SettingField::render(|_, _, _| {
                    Button::new("agent-profile-add")
                        .outline()
                        .label("Add")
                        .on_click(|_, window, cx: &mut App| {
                            open_agent_profile_dialog(None, window, cx);
                        })
                }),
            )
            .description("Create a new agent profile."),
        );

    for (ix, profile) in agent_profiles.iter().enumerate() {
        let label = if profile.name.is_empty() {
            format!("Agent Profile {}", ix + 1)
        } else {
            profile.name.clone()
        };

        group = group.item(
            SettingItem::new(
                label,
                SettingField::render(move |_, _, _| {
                    Button::new(("agent-profile-edit", ix))
                        .outline()
                        .label("Edit")
                        .on_click(move |_, window, cx: &mut App| {
                            open_agent_profile_dialog(Some(ix), window, cx);
                        })
                }),
            )
            .description(agent_kind_label(profile.kind)),
        );
    }
    group
}

/// Open the add/edit dialog for an agent profile. `target` is the index in
/// `AppSettings::agent_profiles` for edit mode, `None` for a new profile.
/// The dialog edits an [`AgentProfileDraft`]; Save commits, Cancel discards.
fn open_agent_profile_dialog(target: Option<usize>, window: &mut Window, cx: &mut App) {
    let profile = match target {
        Some(ix) => cx
            .global::<AppSettings>()
            .agent_profiles
            .get(ix)
            .cloned()
            .unwrap_or_default(),
        // A new profile starts from the Claude Code built-in with a blank
        // name; Save fills in a unique placeholder.
        None => AgentProfile {
            name: String::new(),
            ..builtin_agent_profile(AgentProfileKind::ClaudeCode)
        },
    };
    cx.set_global(AgentProfileDraft { target, profile });

    window.open_dialog(cx, move |dialog, window, _| {
        let title = if target.is_some() {
            "Edit Agent Profile"
        } else {
            "Add Agent Profile"
        };
        let settings_height = window.viewport_size().height;
        let dialog_height = settings_height * 0.6;
        let dialog_top = (settings_height - dialog_height) * 0.5;

        let mut footer = DialogFooter::new()
            .child(DialogClose::new().child(Button::new("agent-profile-cancel").label("Cancel")));

        if let Some(ix) = target {
            footer = footer.child(
                Button::new("agent-profile-delete")
                    .danger()
                    .label("Delete")
                    .on_click(move |_, window, cx: &mut App| {
                        let name = cx.global::<AgentProfileDraft>().profile.name.clone();
                        let subject = if name.is_empty() {
                            "this profile".to_string()
                        } else {
                            format!("profile \"{name}\"")
                        };

                        window.open_alert_dialog(cx, move |alert, _, _| {
                            alert
                                .confirm()
                                .title("Delete Agent Profile")
                                .description(format!("Delete {subject}? This cannot be undone."))
                                .on_ok(move |_, window, cx| {
                                    cx.global_mut::<AppSettings>().remove_agent_profile(ix);
                                    // Pop the confirm and the edit dialog
                                    // explicitly, then return false so the
                                    // alert's own close path does not pop a
                                    // third dialog (the settings one).
                                    window.close_dialog(cx);
                                    window.close_dialog(cx);
                                    false
                                })
                        });
                    }),
            );
        }

        footer = footer.child(
            Button::new("agent-profile-save")
                .primary()
                .label("Save")
                .on_click(|_, window, cx: &mut App| {
                    save_agent_profile_draft(cx);
                    window.close_dialog(cx);
                }),
        );

        dialog
            .title(title)
            .overlay_closable(false)
            .margin_top(dialog_top)
            .w(px(560.))
            .h(dialog_height)
            .content(|content, window, cx| {
                content.overflow_hidden().child(
                    div().flex_1().overflow_hidden().child(
                        v_flex()
                            .size_full()
                            .overflow_y_scrollbar()
                            .child(div().pr_2().child(agent_profile_dialog_content(window, cx))),
                    ),
                )
            })
            .footer(footer)
    });
}

/// Commit the dialog draft into `AppSettings`: dedupe the name, then update
/// the edited entry or append a new one.
fn save_agent_profile_draft(cx: &mut App) {
    let target = cx.global::<AgentProfileDraft>().target;
    let mut profile = cx.global::<AgentProfileDraft>().profile.clone();

    let settings = cx.global_mut::<AppSettings>();
    profile.name = settings.unique_agent_profile_name(&profile.name, profile.kind, target);

    match target {
        Some(ix) => settings.update_agent_profile(ix, profile),
        None => {
            settings.agent_profiles.push(profile);

            // Adding to a previously empty list makes the new profile the
            // default, so NewAgentTab immediately uses it.
            if settings.default_agent_profile.is_empty() {
                settings.default_agent_profile = settings
                    .agent_profiles
                    .last()
                    .map(|p| p.name.clone())
                    .unwrap_or_default();
            }
        }
    }
}

/// One of the two Base Agent choice buttons in the add dialog; the selected
/// kind renders as the primary variant.
fn kind_choice_button(
    id: &'static str,
    kind: AgentProfileKind,
    current: AgentProfileKind,
) -> Button {
    let button = Button::new(id).label(agent_kind_label(kind));
    let button = if kind == current {
        button.primary()
    } else {
        button.outline()
    };

    button.on_click(move |_, _, cx: &mut App| {
        let draft = cx.global_mut::<AgentProfileDraft>();
        if draft.profile.kind == kind {
            return;
        }

        // The executable follows the kind while it still holds a built-in
        // default; a hand-typed path survives the switch.
        let executable = draft.profile.executable.trim();
        if executable.is_empty() || executable == "claude" || executable == "codex" {
            draft.profile.executable = builtin_agent_profile(kind).executable;
        }
        draft.profile.kind = kind;
    })
}

fn agent_profile_dialog_content(window: &mut Window, cx: &mut App) -> Div {
    let profile = cx.global::<AgentProfileDraft>().profile.clone();
    let is_edit = cx.global::<AgentProfileDraft>().target.is_some();

    let kind_label = agent_kind_label(profile.kind);
    let key_env = match profile.kind {
        AgentProfileKind::ClaudeCode => "ANTHROPIC_API_KEY",
        AgentProfileKind::Codex => "OPENAI_API_KEY",
    };
    let endpoint_on = profile.use_custom_endpoint;

    let name_input = card_text_input(
        "agent-profile-dialog-name".to_string(),
        profile.name.clone().into(),
        false,
        |value, cx| cx.global_mut::<AgentProfileDraft>().profile.name = value,
        window,
        cx,
    );

    let exe_input = card_text_input(
        "agent-profile-dialog-exe".to_string(),
        profile.executable.clone().into(),
        false,
        |value, cx| cx.global_mut::<AgentProfileDraft>().profile.executable = value,
        window,
        cx,
    );

    let model_input = card_text_input(
        "agent-profile-dialog-model".to_string(),
        profile.model.clone().into(),
        false,
        |value, cx| cx.global_mut::<AgentProfileDraft>().profile.model = value,
        window,
        cx,
    );

    let url_input = card_text_input(
        "agent-profile-dialog-url".to_string(),
        profile.api_base_url.clone().into(),
        false,
        |value, cx| cx.global_mut::<AgentProfileDraft>().profile.api_base_url = value,
        window,
        cx,
    );

    let key_input = card_text_input(
        "agent-profile-dialog-key".to_string(),
        profile.api_key.clone().into(),
        false,
        |value, cx| cx.global_mut::<AgentProfileDraft>().profile.api_key = value,
        window,
        cx,
    );

    let kind_control: AnyElement = if is_edit {
        // The kind decides the wire protocol; changing it under an existing
        // profile would silently repurpose tabs and persisted state, so it
        // is fixed after creation.
        Label::new(kind_label).text_sm().into_any_element()
    } else {
        h_flex()
            .gap_2()
            .child(kind_choice_button(
                "agent-profile-kind-claude",
                AgentProfileKind::ClaudeCode,
                profile.kind,
            ))
            .child(kind_choice_button(
                "agent-profile-kind-codex",
                AgentProfileKind::Codex,
                profile.kind,
            ))
            .into_any_element()
    };

    let endpoint_switch = Switch::new("agent-profile-dialog-endpoint")
        .checked(endpoint_on)
        .on_click(|checked: &bool, _, cx: &mut App| {
            cx.global_mut::<AgentProfileDraft>()
                .profile
                .use_custom_endpoint = *checked;
        });

    let mut env_rows = v_flex().w_full().gap_2();
    for (row, var) in profile.env.iter().enumerate() {
        let env_name_input = card_text_input(
            format!("agent-profile-dialog-env-{row}-name"),
            var.name.clone().into(),
            false,
            move |value, cx| {
                if let Some(var) = cx
                    .global_mut::<AgentProfileDraft>()
                    .profile
                    .env
                    .get_mut(row)
                {
                    var.name = value;
                }
            },
            window,
            cx,
        );

        let env_value_input = card_text_input(
            format!("agent-profile-dialog-env-{row}-value"),
            var.value.clone().into(),
            false,
            move |value, cx| {
                if let Some(var) = cx
                    .global_mut::<AgentProfileDraft>()
                    .profile
                    .env
                    .get_mut(row)
                {
                    var.value = value;
                }
            },
            window,
            cx,
        );

        env_rows = env_rows.child(
            h_flex()
                .w_full()
                .gap_2()
                .child(Input::new(&env_name_input).flex_1())
                .child(Input::new(&env_value_input).flex_1())
                .child(
                    Button::new(SharedString::from(format!(
                        "agent-profile-dialog-env-remove-{row}"
                    )))
                    .outline()
                    .label("Remove")
                    .on_click(move |_, _, cx: &mut App| {
                        let env = &mut cx.global_mut::<AgentProfileDraft>().profile.env;
                        if row < env.len() {
                            env.remove(row);
                        }
                    }),
                ),
        );
    }

    let env_section = v_flex()
        .w_full()
        .gap_2()
        .child(Label::new("Environment Variables").text_sm())
        .child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("Extra environment variables applied to the agent process."),
        )
        .child(env_rows)
        .child(
            h_flex().child(
                Button::new("agent-profile-dialog-env-add")
                    .outline()
                    .label("Add Variable")
                    .on_click(|_, _, cx: &mut App| {
                        cx.global_mut::<AgentProfileDraft>()
                            .profile
                            .env
                            .push(EnvVar::default());
                    }),
            ),
        );

    v_flex()
        .w_full()
        .gap_4()
        .child(card_row(
            "Name",
            "Display name; it keys the default selector and per-profile settings.",
            Input::new(&name_input).w_64(),
            cx,
        ))
        .child(card_row(
            "Base Agent",
            "Which agent CLI this profile launches.",
            kind_control,
            cx,
        ))
        .child(card_row(
            "Executable Path",
            "Executable name or full path; a bare name resolves via PATH.",
            Input::new(&exe_input).w_64(),
            cx,
        ))
        .child(card_row(
            "Model",
            match profile.kind {
                AgentProfileKind::ClaudeCode => {
                    "Initial model; passed to Claude Code as ANTHROPIC_MODEL."
                }
                AgentProfileKind::Codex => {
                    "Initial model; passed to Codex when its app-server thread starts."
                }
            },
            Input::new(&model_input).w_64(),
            cx,
        ))
        .child(card_row(
            "Use Custom API Endpoint",
            "Route this agent through your own API endpoint.",
            endpoint_switch,
            cx,
        ))
        .child(card_row(
            "API URL",
            match profile.kind {
                AgentProfileKind::ClaudeCode => {
                    "Exported as ANTHROPIC_BASE_URL while the custom endpoint is enabled."
                        .to_string()
                }
                AgentProfileKind::Codex => {
                    "Injected as a profile-scoped Codex model provider base URL.".to_string()
                }
            },
            Input::new(&url_input).disabled(!endpoint_on).w_64(),
            cx,
        ))
        .child(card_row(
            "API Key",
            match profile.kind {
                AgentProfileKind::ClaudeCode => {
                    format!("Exported as {key_env} while the custom endpoint is enabled.")
                }
                AgentProfileKind::Codex => {
                    format!("Exported as {key_env} and referenced by the profile-scoped provider.")
                }
            },
            Input::new(&key_input).disabled(!endpoint_on).w_64(),
            cx,
        ))
        .child(env_section)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_shape_dropdown_values_match_config_shapes() {
        assert_eq!(cursor_shape_from_value("block"), CursorShape::Block);
        assert_eq!(cursor_shape_from_value("line"), CursorShape::Beam);
        assert_eq!(cursor_shape_from_value("underline"), CursorShape::Underline);
    }

    #[test]
    fn tab_width_clamps_to_allowed_range() {
        assert_eq!(clamp_tab_width(DEFAULT_TAB_WIDTH), DEFAULT_TAB_WIDTH);
        assert_eq!(clamp_tab_width(200.0), 200.0);
        assert_eq!(clamp_tab_width(MAX_TAB_WIDTH), MAX_TAB_WIDTH);
        assert_eq!(clamp_tab_width(10.0), DEFAULT_TAB_WIDTH);
        assert_eq!(clamp_tab_width(9999.0), MAX_TAB_WIDTH);
        assert_eq!(clamp_tab_width(f64::NAN), DEFAULT_TAB_WIDTH);
    }

    #[test]
    fn ui_font_falls_back_when_blank() {
        assert_eq!(ui_font_or_default("Cascadia Code"), "Cascadia Code");
        assert_eq!(ui_font_or_default(""), DEFAULT_UI_FONT);
        assert_eq!(ui_font_or_default("   "), DEFAULT_UI_FONT);
    }

    #[test]
    fn terminal_font_falls_back_when_blank() {
        assert_eq!(terminal_font_or_default("Cascadia Code"), "Cascadia Code");
        assert_eq!(terminal_font_or_default(""), DEFAULT_FONT_FAMILY);
        assert_eq!(terminal_font_or_default("   "), DEFAULT_FONT_FAMILY);
    }

    #[test]
    fn terminal_font_metrics_clamp_to_allowed_range() {
        assert_eq!(clamp_terminal_font_size(16.0), 16.0);
        assert_eq!(clamp_terminal_font_size(1.0), 6.0);
        assert_eq!(clamp_terminal_font_size(100.0), 72.0);
        assert_eq!(clamp_terminal_font_size(f64::NAN), DEFAULT_FONT_SIZE);

        assert_eq!(clamp_terminal_line_height(1.2), 1.2);
        assert_eq!(clamp_terminal_line_height(0.1), 0.8);
        assert_eq!(clamp_terminal_line_height(5.0), 3.0);
        assert_eq!(clamp_terminal_line_height(f64::NAN), DEFAULT_LINE_HEIGHT);
    }

    #[test]
    fn window_transparency_controls_opacity_and_blur() {
        assert_eq!(clamp_background_opacity(0.1), 0.2);
        assert_eq!(clamp_background_opacity(0.65), 0.65);
        assert_eq!(clamp_background_opacity(2.0), 1.0);
        assert_eq!(clamp_background_opacity(f64::NAN), 1.0);
        assert_eq!(effective_background_opacity(false, 0.65), 1.0);
        assert_eq!(effective_background_opacity(true, 0.65), 0.65);
        assert_eq!(clamp_background_image_opacity(-1.0), 0.0);
        assert_eq!(clamp_background_image_opacity(2.0), 1.0);
        assert_eq!(
            clamp_background_image_opacity(f64::NAN),
            DEFAULT_BACKGROUND_IMAGE_OPACITY
        );
        assert_eq!(effective_surface_background_opacity(1.0, None), 1.0);
        assert!((effective_surface_background_opacity(1.0, Some(0.3)) - 0.7).abs() < 1e-12);
        assert_eq!(effective_background_image_layer_opacity(1.0, 0.0), 0.0);
        assert!((effective_background_image_layer_opacity(1.0, 0.3) - 1.0).abs() < 1e-12);
        let surface = effective_surface_background_opacity(0.65, Some(0.3));
        let image = effective_background_image_layer_opacity(0.65, 0.3);
        assert!((surface + (1.0 - surface) * image - 0.65).abs() < 1e-12);
        assert_eq!(
            window_background_appearance_for(true),
            WindowBackgroundAppearance::Blurred
        );
        assert_eq!(
            window_background_appearance_for(false),
            WindowBackgroundAppearance::Opaque
        );
    }

    #[test]
    fn git_interval_clamps_to_allowed_set() {
        for v in [10, 15, 30, 60] {
            assert_eq!(clamp_git_interval(v), v);
        }
        for v in [0, 7, 45, 1000] {
            assert_eq!(clamp_git_interval(v), 30);
        }
    }

    #[test]
    fn input_style_value_roundtrip() {
        for style in [InputStyle::Waterfall, InputStyle::FixedBottom] {
            assert_eq!(input_style_from_value(style.as_str()), style);
        }
        // Unknown values fall back to the default style.
        assert_eq!(input_style_from_value("bogus"), InputStyle::Waterfall);
    }

    #[test]
    fn load_falls_back_to_default_profile() {
        // Test env has no config file: defaults apply, the empty profiles
        // list maps to the single built-in profile, and the unset default
        // profile resolves to that profile's name.
        let settings = AppSettings::load();
        assert_eq!(settings.input_style, InputStyle::Waterfall);
        assert_eq!(settings.profiles.len(), 1);
        assert_eq!(settings.default_profile, settings.profiles[0].name);
        assert_eq!(settings.default_profile, "PowerShell");
        assert!(settings.monospace_only);
        assert!(settings.restore_last_session_when_opening);
    }

    #[test]
    fn default_profile_command_resolves_by_name() {
        let mut settings = AppSettings::default();
        settings.profiles = vec![
            Profile {
                name: "PowerShell".into(),
                shell: DEFAULT_SHELL.into(),
                args: String::new(),
            },
            Profile {
                name: "Cmd".into(),
                shell: "cmd.exe".into(),
                args: "/k echo hi".into(),
            },
        ];
        settings.default_profile = "Cmd".into();

        let (shell, args) = settings.default_profile_command();
        assert_eq!(shell.as_deref(), Some("cmd.exe"));
        assert_eq!(args, vec!["/k", "echo", "hi"]);

        // Dangling name falls back to the first profile.
        settings.default_profile = "Nope".into();
        let (shell, _) = settings.default_profile_command();
        assert_eq!(shell.as_deref(), Some(DEFAULT_SHELL));

        // Blank shell path: no override, session uses its built-in default.
        settings.profiles[0].shell = "  ".into();
        settings.default_profile = "PowerShell".into();
        let (shell, args) = settings.default_profile_command();
        assert!(shell.is_none());
        assert!(args.is_empty());
    }

    #[test]
    fn profile_name_resolves_from_launch_command() {
        let mut settings = AppSettings::default();
        settings.profiles.push(Profile {
            name: "Developer PowerShell".into(),
            shell: "pwsh.exe".into(),
            args: "-NoLogo".into(),
        });

        assert_eq!(
            settings.profile_name_for_command(Some("PWSH.EXE"), &["-NoLogo".to_string()]),
            "Developer PowerShell"
        );
    }

    #[test]
    fn profile_mutations_keep_default_valid() {
        let mut settings = AppSettings::default();

        // Add: unique placeholder names.
        settings.add_profile();
        settings.add_profile();
        assert_eq!(settings.profiles.len(), 3);
        assert_eq!(settings.profiles[1].name, "Profile 2");
        assert_eq!(settings.profiles[2].name, "Profile 3");

        // Rename the default: the reference follows.
        settings.rename_profile(0, "Pwsh".into());
        assert_eq!(settings.default_profile, "Pwsh");

        // Remove the default: falls back to the first remaining profile.
        settings.remove_profile(0);
        assert_eq!(settings.default_profile, "Profile 2");

        // The last profile cannot be removed.
        settings.remove_profile(0);
        settings.remove_profile(0);
        assert_eq!(settings.profiles.len(), 1);
    }

    #[test]
    fn agent_profile_mutations_keep_default_valid() {
        let mut settings = AppSettings::default();
        assert_eq!(settings.agent_profiles.len(), 2);
        assert_eq!(settings.default_agent_profile, "Claude Code");

        // Unique-name resolution: an empty desired name takes the kind
        // label, collisions get a numeric suffix, and the excluded index
        // (edit mode) keeps its own name available.
        assert_eq!(
            settings.unique_agent_profile_name("", AgentProfileKind::ClaudeCode, None),
            "Claude Code 2"
        );
        assert_eq!(
            settings.unique_agent_profile_name("Codex", AgentProfileKind::Codex, Some(1)),
            "Codex"
        );
        assert_eq!(
            settings.unique_agent_profile_name(" Mine ", AgentProfileKind::Codex, None),
            "Mine"
        );

        // Update with a rename: the default reference follows.
        let renamed = AgentProfile {
            name: "Proxy".into(),
            ..settings.agent_profiles[0].clone()
        };
        settings.update_agent_profile(0, renamed);
        assert_eq!(settings.default_agent_profile, "Proxy");

        // Remove the default: falls back to the first remaining profile.
        settings.remove_agent_profile(0);
        assert_eq!(settings.default_agent_profile, "Codex");

        // Every profile can be removed; an empty list clears the default.
        settings.remove_agent_profile(0);
        assert!(settings.agent_profiles.is_empty());
        assert_eq!(settings.default_agent_profile, "");

        // The shortcut fallback still produces a launchable profile.
        assert_eq!(
            settings.default_agent_profile_entry().kind,
            AgentProfileKind::ClaudeCode
        );
    }

    #[test]
    fn default_agent_profile_entry_resolves_by_name() {
        let mut settings = AppSettings::default();
        settings.agent_profiles[1].executable = "custom-codex".into();
        settings.default_agent_profile = "Codex".into();
        assert_eq!(
            settings.default_agent_profile_entry().executable,
            "custom-codex"
        );

        // Dangling name falls back to the first profile.
        settings.default_agent_profile = "Nope".into();
        assert_eq!(
            settings.default_agent_profile_entry().kind,
            AgentProfileKind::ClaudeCode
        );
    }

    #[test]
    fn defaults_have_one_powershell_profile() {
        let settings = AppSettings::default();
        assert_eq!(settings.input_style, InputStyle::Waterfall);
        assert_eq!(settings.profiles.len(), 1);
        assert_eq!(settings.profiles[0].shell, DEFAULT_SHELL);
        assert_eq!(settings.profiles[0].args, "");
        assert!(settings.restore_last_session_when_opening);
    }
}
