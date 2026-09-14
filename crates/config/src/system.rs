//! System-behavior settings, persisted as the `[system]` section of
//! `config.toml` by the settings dialog (System page).

use serde::{Deserialize, Serialize};

use crate::defaults::default_bool_true;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum WarnBeforeTerminatingShell {
    Disabled,
    #[default]
    WhenChildProcessesRunning,
    Always,
}

impl WarnBeforeTerminatingShell {
    pub fn should_warn(self, child_process_count: usize) -> bool {
        match self {
            Self::Disabled => false,
            Self::WhenChildProcessesRunning => child_process_count > 0,
            Self::Always => true,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum NewlineShortcut {
    #[default]
    CtrlEnter,
    ShiftEnter,
    Off,
}

/// The `[system]` section: process/system behavior settings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SystemConfig {
    /// Reopen the last saved workspace/tab session on startup.
    #[serde(
        default = "default_bool_true",
        rename = "restore-last-session-when-opening"
    )]
    pub restore_last_session_when_opening: bool,

    /// Manage each tab's shell with a Windows Job Object (kill tree on close).
    #[serde(default, rename = "manage-subprocess-job")]
    pub manage_subprocess_job: bool,

    /// When to warn before closing a pane, tab, workspace, or window.
    #[serde(default, rename = "warn-before-terminating-shell")]
    pub warn_before_terminating_shell: WarnBeforeTerminatingShell,

    /// Ask for confirmation before closing a workspace, Agent tab, or window.
    #[serde(
        default = "default_bool_true",
        rename = "confirm-before-closing-workspace"
    )]
    pub confirm_before_closing_workspace: bool,

    /// Raise the main (UI) and render thread priority to AboveNormal.
    #[serde(default, rename = "prioritize-ui-threads")]
    pub prioritize_ui_threads: bool,

    /// Modified Enter key that inserts a new line without submitting input.
    #[serde(default, rename = "newline-shortcut")]
    pub newline_shortcut: NewlineShortcut,

    /// Open an Explorer directory in the deepest workspace that already
    /// contains it, instead of always opening a workspace of its own.
    #[serde(default = "default_bool_true", rename = "open-in-best-workspace")]
    pub open_in_best_workspace: bool,

    /// Allow the application to send native notifications.
    #[serde(default = "default_bool_true", rename = "send-system-notifications")]
    pub send_system_notifications: bool,
}

impl Default for SystemConfig {
    fn default() -> Self {
        Self {
            restore_last_session_when_opening: true,
            manage_subprocess_job: false,
            warn_before_terminating_shell: WarnBeforeTerminatingShell::default(),
            confirm_before_closing_workspace: true,
            prioritize_ui_threads: false,
            newline_shortcut: NewlineShortcut::default(),
            open_in_best_workspace: true,
            send_system_notifications: true,
        }
    }
}

impl From<WarnBeforeTerminatingShell> for &'static str {
    fn from(value: WarnBeforeTerminatingShell) -> Self {
        match value {
            WarnBeforeTerminatingShell::Disabled => "disabled",
            WarnBeforeTerminatingShell::WhenChildProcessesRunning => "when-child-processes-running",
            WarnBeforeTerminatingShell::Always => "always",
        }
    }
}

impl From<&str> for WarnBeforeTerminatingShell {
    fn from(value: &str) -> Self {
        match value {
            "disabled" => Self::Disabled,
            "always" => Self::Always,
            _ => Self::WhenChildProcessesRunning,
        }
    }
}

impl From<NewlineShortcut> for &'static str {
    fn from(value: NewlineShortcut) -> Self {
        match value {
            NewlineShortcut::CtrlEnter => "ctrl-enter",
            NewlineShortcut::ShiftEnter => "shift-enter",
            NewlineShortcut::Off => "off",
        }
    }
}

impl From<&str> for NewlineShortcut {
    fn from(value: &str) -> Self {
        match value {
            "shift-enter" => Self::ShiftEnter,
            "off" => Self::Off,
            _ => Self::CtrlEnter,
        }
    }
}
