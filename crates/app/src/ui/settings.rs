//! App settings: the global settings model and the settings dialog content,
//! built on gpui-component's `Settings` two-pane framework (page sidebar on the
//! left, setting groups on the right).
//!
//! Persisted to `config.toml`: seeded via [`AppSettings::load`] at startup,
//! written back patch-style via [`AppSettings::save`] once when the settings
//! dialog closes (see `Shell::on_show_settings`). Field edits mutate the global
//! live for preview; only closing the dialog persists them.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, Entity, FileDialogFilter, Global, ParentElement as _, PathPromptOptions,
    SharedString, Styled as _, px, relative,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::group_box::GroupBoxVariant;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::setting::{
    NumberFieldOptions, SettingField, SettingGroup, SettingItem, SettingPage, Settings,
};
use gpui_component::{AxisExt as _, Disableable as _, Sizable as _, h_flex, v_flex};

/// Default shell for a new profile.
pub const DEFAULT_SHELL: &str = r"C:\WINDOWS\System32\WindowsPowerShell\v1.0\powershell.exe";

/// Used when the config sets no font family.
pub const DEFAULT_FONT_FAMILY: &str = "Consolas";
pub const DEFAULT_FONT_SIZE: f64 = 14.0;
pub const DEFAULT_LINE_HEIGHT: f64 = 1.0;

/// Font family for the app chrome (titlebar, sidebar, tabs, dialogs), used
/// when the config sets none.
pub const DEFAULT_UI_FONT: &str = "Segoe UI";

/// Fixed tab width in pixels; the setting ranges from this value to 3x it.
pub const DEFAULT_TAB_WIDTH: f64 = 120.0;
pub const MAX_TAB_WIDTH: f64 = DEFAULT_TAB_WIDTH * 3.0;

/// Initial terminal font. Live changes then go through
/// `AppSettings.terminal_font_family`.
fn initial_font_family() -> SharedString {
    DEFAULT_FONT_FAMILY.into()
}

/// How the prompt input is presented.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InputStyle {
    /// Prompt flows inline with the terminal output.
    Waterfall,
    /// Prompt is presented at a fixed bottom row.
    FixedBottom,
}

impl InputStyle {
    /// Stable identifier used as the dropdown value.
    fn value(self) -> &'static str {
        match self {
            Self::Waterfall => "waterfall",
            Self::FixedBottom => "fixed-bottom",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Waterfall => "Waterfall",
            Self::FixedBottom => "Fixed Bottom",
        }
    }

    pub(crate) fn is_fixed_bottom(self) -> bool {
        matches!(self, Self::FixedBottom)
    }

    fn from_value(value: &str) -> Self {
        match value {
            "waterfall" => Self::Waterfall,
            "fixed-bottom" => Self::FixedBottom,
            _ => Self::Waterfall,
        }
    }

    fn from_config(value: nmt_config::appearance::InputStyle) -> Self {
        match value {
            nmt_config::appearance::InputStyle::Waterfall => Self::Waterfall,
            nmt_config::appearance::InputStyle::FixedBottom => Self::FixedBottom,
        }
    }

    fn to_config(self) -> nmt_config::appearance::InputStyle {
        match self {
            Self::Waterfall => nmt_config::appearance::InputStyle::Waterfall,
            Self::FixedBottom => nmt_config::appearance::InputStyle::FixedBottom,
        }
    }
}

/// One shell profile: the executable to launch and its arguments.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Profile {
    /// Display name; also how `default_profile` references a profile.
    pub name: String,
    pub shell: String,
    /// Command-line arguments, space-separated as typed.
    pub args: String,
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            name: "PowerShell".to_string(),
            shell: DEFAULT_SHELL.to_string(),
            args: String::new(),
        }
    }
}

/// The app-wide settings model, stored as a gpui global.
pub struct AppSettings {
    pub input_style: InputStyle,
    pub profiles: Vec<Profile>,
    /// Name of the profile new terminals use. Always references an existing
    /// profile by name (seeded to the first profile when unset).
    pub default_profile: String,
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
    /// Fixed tab width in pixels (DEFAULT_TAB_WIDTH..=MAX_TAB_WIDTH).
    pub tab_width: f64,
    /// Filter the settings font picker to monospace fonts.
    pub monospace_only: bool,
    /// Restore the last saved workspace/tab session on startup.
    pub restore_last_session_when_opening: bool,
    /// Manage each tab's shell with a Windows Job Object: closing the tab
    /// kills the shell's entire process tree. Applies to new tabs.
    pub manage_subprocess_job: bool,
    /// Warn before closing a tab/workspace whose shell has child processes.
    /// Effective only with `manage_subprocess_job` (the Job is the detector).
    pub warn_before_terminating_shell: bool,
    /// Ask for confirmation before closing a workspace.
    pub confirm_before_closing_workspace: bool,
    /// Raise the main (UI) and render thread priority to AboveNormal.
    pub prioritize_ui_threads: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            input_style: InputStyle::Waterfall,
            profiles: vec![Profile::default()],
            default_profile: Profile::default().name,
            command_blocks: true,
            show_daily_token_usage: false,
            show_git_status_on_title_bar: false,
            git_status_refresh_interval: 30,
            ui_font_family: DEFAULT_UI_FONT.into(),
            terminal_font_family: initial_font_family(),
            terminal_font_size: DEFAULT_FONT_SIZE,
            terminal_line_height: DEFAULT_LINE_HEIGHT,
            tab_width: DEFAULT_TAB_WIDTH,
            monospace_only: true,
            restore_last_session_when_opening: true,
            manage_subprocess_job: false,
            warn_before_terminating_shell: true,
            confirm_before_closing_workspace: true,
            prioritize_ui_threads: false,
        }
    }
}

impl Global for AppSettings {}

struct ShellPathFieldState {
    input: Entity<InputState>,
    _subscription: gpui::Subscription,
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

impl AppSettings {
    /// Build from the loaded config file: the `[appearance]`, `[system]`, and
    /// `[[profiles]]` sections, falling back to the built-in defaults.
    pub fn load() -> Self {
        let config = nmt_config::get();
        let appearance = &config.appearance;
        let profiles: Vec<Profile> = if config.profiles.list.is_empty() {
            vec![Profile::default()]
        } else {
            config
                .profiles
                .list
                .iter()
                .map(|p| Profile {
                    name: p.name.clone(),
                    shell: p.shell.clone(),
                    args: p.args.clone(),
                })
                .collect()
        };
        // An unset or dangling default falls back to the first profile.
        let default_profile = if profiles.iter().any(|p| p.name == config.profiles.default) {
            config.profiles.default.clone()
        } else {
            profiles[0].name.clone()
        };
        Self {
            input_style: InputStyle::from_config(appearance.input_style),
            profiles,
            default_profile,
            command_blocks: appearance.command_blocks,
            show_daily_token_usage: appearance.show_daily_token_usage,
            show_git_status_on_title_bar: appearance.show_git_status_on_title_bar,
            git_status_refresh_interval: clamp_git_interval(appearance.git_status_refresh_interval),
            ui_font_family: ui_font_or_default(&appearance.ui_font),
            terminal_font_family: terminal_font_or_default(&appearance.terminal_font_family),
            terminal_font_size: clamp_terminal_font_size(appearance.terminal_font_size),
            terminal_line_height: clamp_terminal_line_height(appearance.terminal_line_height),
            tab_width: clamp_tab_width(appearance.tab_width),
            monospace_only: appearance.monospace_only,
            restore_last_session_when_opening: config.system.restore_last_session_when_opening,
            manage_subprocess_job: config.system.manage_subprocess_job,
            warn_before_terminating_shell: config.system.warn_before_terminating_shell,
            confirm_before_closing_workspace: config.system.confirm_before_closing_workspace,
            prioritize_ui_threads: config.system.prioritize_ui_threads,
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
            ..Profile::default()
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
    pub fn rename_profile(&mut self, ix: usize, name: String) {
        if self.profiles[ix].name == self.default_profile {
            self.default_profile = name.clone();
        }
        self.profiles[ix].name = name;
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

    /// Persist the dialog-managed settings into `config.toml` (patch-style,
    /// preserving unrelated content). Called once on dialog close. Failures are
    /// logged, never fatal.
    pub fn save(&self) {
        let appearance = nmt_config::appearance::AppearanceConfig {
            input_style: self.input_style.to_config(),
            command_blocks: self.command_blocks,
            show_daily_token_usage: self.show_daily_token_usage,
            show_git_status_on_title_bar: self.show_git_status_on_title_bar,
            git_status_refresh_interval: self.git_status_refresh_interval,
            tab_width: self.tab_width,
            ui_font: self.ui_font_family.to_string(),
            terminal_font_family: self.terminal_font_family.to_string(),
            terminal_font_size: self.terminal_font_size,
            terminal_line_height: self.terminal_line_height,
            monospace_only: self.monospace_only,
        };
        let system = nmt_config::system::SystemConfig {
            restore_last_session_when_opening: self.restore_last_session_when_opening,
            manage_subprocess_job: self.manage_subprocess_job,
            warn_before_terminating_shell: self.warn_before_terminating_shell,
            confirm_before_closing_workspace: self.confirm_before_closing_workspace,
            prioritize_ui_threads: self.prioritize_ui_threads,
        };
        let profiles: Vec<nmt_config::profile::Profile> = self
            .profiles
            .iter()
            .map(|p| nmt_config::profile::Profile {
                name: p.name.clone(),
                shell: p.shell.clone(),
                args: p.args.clone(),
            })
            .collect();
        if let Err(err) = nmt_config::appearance::save_settings(
            &appearance,
            &system,
            &profiles,
            &self.default_profile,
        ) {
            tracing::warn!("failed to save settings to config.toml: {err}");
        }
    }
}

/// The settings dialog body: a two-pane `Settings` view with a single
/// "Terminal" page holding the Input Style dropdown and the profile fields.
/// Rebuilt every render; the field closures read/write the `AppSettings`
/// global directly.
pub fn settings_view(cx: &App) -> Settings {
    let profiles = cx.global::<AppSettings>().profiles.clone();
    let job_enabled = cx.global::<AppSettings>().manage_subprocess_job;
    Settings::new("app-settings")
        .sidebar_width(px(160.0))
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
                                        InputStyle::Waterfall.value().into(),
                                        InputStyle::Waterfall.label().into(),
                                    ),
                                    (
                                        InputStyle::FixedBottom.value().into(),
                                        InputStyle::FixedBottom.label().into(),
                                    ),
                                ],
                                |cx| cx.global::<AppSettings>().input_style.value().into(),
                                |value, cx| {
                                    cx.global_mut::<AppSettings>().input_style =
                                        InputStyle::from_value(&value);
                                },
                            )
                            .default_value(SharedString::from(InputStyle::Waterfall.value())),
                        )
                        .description("How the prompt input is presented."),
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
                             exit status, and duration.",
                        ),
                    ),
            ),
        )
        .page(
            SettingPage::new("Appearance")
                .default_open(true)
                .group(
                    SettingGroup::new().title("Interface").item(
                        SettingItem::new(
                            "UI Font",
                            crate::ui::font_picker::font_family_field(
                                crate::ui::font_picker::FontTarget::Ui,
                            ),
                        )
                        .description("Font for the app chrome (titlebar, sidebar, tabs, dialogs)."),
                    ),
                )
                .group(
                    SettingGroup::new()
                        .title("Terminal Font")
                        .item(
                            SettingItem::new(
                                "Font Family",
                                crate::ui::font_picker::font_family_field(
                                    crate::ui::font_picker::FontTarget::Terminal,
                                ),
                            )
                            .description("Font used by the terminal view."),
                        )
                        .item(
                            SettingItem::new(
                                "Font Size",
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
                                "Line Height",
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
        .page(profiles_page(&profiles))
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
                                SettingField::switch(
                                    |cx| cx.global::<AppSettings>().warn_before_terminating_shell,
                                    |value, cx| {
                                        cx.global_mut::<AppSettings>()
                                            .warn_before_terminating_shell = value;
                                    },
                                ),
                            )
                            // The Job Object is the child-process detector, so this
                            // is meaningless (and disabled) without it.
                            .disabled(!job_enabled)
                            .description(
                                "Ask before closing a tab or workspace whose shell has \
                         running child processes. Requires Job management.",
                            ),
                        ),
                )
                .group(
                    SettingGroup::new()
                        .title("Windows")
                        .item(
                            SettingItem::new(
                                "Enable Windows Context Menu",
                                SettingField::switch(
                                    |_| nmt_platform::is_shell_integration_registered(),
                                    |value, _| {
                                        if let Err(err) =
                                            nmt_platform::register_with_elevated(value)
                                        {
                                            tracing::warn!(
                                                "failed to toggle Windows context menu: {err:#}"
                                            );
                                        }
                                    },
                                ),
                            )
                            .description("Add NiumaTerm actions to File Explorer directory menus."),
                        )
                        .item(
                            SettingItem::new(
                                "Enable System Notification",
                                SettingField::switch(
                                    |_| nmt_platform::system_notification_enabled(),
                                    |value, _| {
                                        if let Err(err) =
                                            nmt_platform::set_system_notification_enabled(value)
                                        {
                                            tracing::warn!(
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
                                    cx.global::<crate::PlatformHandle>()
                                        .0
                                        .set_ui_thread_priority(value);
                                },
                            ),
                        )
                        .description("Raise the main and render thread priority to AboveNormal."),
                    ),
                ),
        )
}

/// The Profiles page: default selector and add button on top, then one card
/// (group) per profile with its fields and a remove row.
fn profiles_page(profiles: &[Profile]) -> SettingPage {
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

    let mut page = SettingPage::new("Profiles").default_open(true).group(
        SettingGroup::new()
            .title("Profiles")
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
            ),
    );

    let count = profiles.len();
    for (ix, profile) in profiles.iter().enumerate() {
        let title = if profile.name.is_empty() {
            format!("Profile {}", ix + 1)
        } else {
            profile.name.clone()
        };
        page = page.group(
            SettingGroup::new()
                // Rounded outline box so each profile reads as one card.
                .variant(GroupBoxVariant::Outline)
                .title(title)
                .item(
                    SettingItem::new(
                        "Name",
                        SettingField::input(
                            move |cx| cx.global::<AppSettings>().profiles[ix].name.clone().into(),
                            move |value, cx| {
                                cx.global_mut::<AppSettings>()
                                    .rename_profile(ix, value.to_string());
                            },
                        ),
                    )
                    .description("Display name; the card title and default selector follow it."),
                )
                .item(
                    SettingItem::new(
                        "Shell Path",
                        shell_path_field(ix).on_reset(
                            move |cx| {
                                cx.global::<AppSettings>().profiles[ix].shell != DEFAULT_SHELL
                            },
                            move |_, cx| {
                                cx.global_mut::<AppSettings>().profiles[ix].shell =
                                    DEFAULT_SHELL.to_string();
                            },
                        ),
                    )
                    .description("Path to the shell executable."),
                )
                .item(
                    SettingItem::new(
                        "Arguments",
                        SettingField::input(
                            move |cx| cx.global::<AppSettings>().profiles[ix].args.clone().into(),
                            move |value, cx| {
                                cx.global_mut::<AppSettings>().profiles[ix].args =
                                    value.to_string();
                            },
                        )
                        .default_value(SharedString::from("")),
                    )
                    .description("Command-line arguments, space-separated."),
                )
                .item(
                    SettingItem::new(
                        "Remove Profile",
                        SettingField::render(move |_, _, _| {
                            Button::new(("profile-remove", ix))
                                .danger()
                                .label("Remove")
                                .disabled(count <= 1)
                                .on_click(move |_, _, cx: &mut App| {
                                    cx.global_mut::<AppSettings>().remove_profile(ix);
                                })
                        }),
                    )
                    .description(if count <= 1 {
                        "The last profile cannot be removed."
                    } else {
                        "Removing the default falls back to the first profile."
                    }),
                ),
        );
    }
    page
}

fn shell_path_field(ix: usize) -> SettingField<SharedString> {
    SettingField::render(move |options, window, cx| {
        let value = SharedString::from(cx.global::<AppSettings>().profiles[ix].shell.clone());
        let state =
            window.use_keyed_state(SharedString::from(format!("shell-path-state-{ix}")), cx, {
                let value = value.clone();
                move |window, cx| {
                    let input = cx.new(|cx| InputState::new(window, cx).default_value(value));
                    let _subscription = cx.subscribe(&input, move |_, input, event, cx| {
                        if matches!(event, InputEvent::Change) {
                            let value = input.read(cx).value().to_string();
                            if let Some(profile) =
                                cx.global_mut::<AppSettings>().profiles.get_mut(ix)
                            {
                                profile.shell = value;
                            }
                        }
                    });

                    ShellPathFieldState {
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

        let browse_input = input.clone();
        v_flex()
            .gap_2()
            .map(|this| {
                if options.layout.is_horizontal() {
                    this.w_64()
                } else {
                    this.w_full()
                }
            })
            .child(
                Input::new(&input)
                    .disabled(options.disabled)
                    .with_size(options.size)
                    .w_full(),
            )
            .child(
                h_flex().w_full().justify_end().child(
                    Button::new(("profile-shell-browse", ix))
                        .outline()
                        .label("Browse")
                        .disabled(options.disabled)
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
            )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
            assert_eq!(InputStyle::from_value(style.value()), style);
        }
        // Unknown values fall back to the default style.
        assert_eq!(InputStyle::from_value("bogus"), InputStyle::Waterfall);
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
    fn defaults_have_one_powershell_profile() {
        let settings = AppSettings::default();
        assert_eq!(settings.input_style, InputStyle::Waterfall);
        assert_eq!(settings.profiles.len(), 1);
        assert_eq!(settings.profiles[0].shell, DEFAULT_SHELL);
        assert_eq!(settings.profiles[0].args, "");
        assert!(settings.restore_last_session_when_opening);
    }
}
