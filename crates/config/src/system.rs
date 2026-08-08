//! System-behavior settings, persisted as the `[system]` section of
//! `config.toml` by the settings dialog (System page).

use serde::{Deserialize, Serialize};
use toml_edit::{DocumentMut, value};

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
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::WhenChildProcessesRunning => "when-child-processes-running",
            Self::Always => "always",
        }
    }

    pub fn from_value(value: &str) -> Self {
        match value {
            "disabled" => Self::Disabled,
            "always" => Self::Always,
            _ => Self::WhenChildProcessesRunning,
        }
    }

    pub fn should_warn(self, child_process_count: usize) -> bool {
        match self {
            Self::Disabled => false,
            Self::WhenChildProcessesRunning => child_process_count > 0,
            Self::Always => true,
        }
    }
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
}

impl Default for SystemConfig {
    fn default() -> Self {
        Self {
            restore_last_session_when_opening: true,
            manage_subprocess_job: false,
            warn_before_terminating_shell: WarnBeforeTerminatingShell::default(),
            confirm_before_closing_workspace: true,
            prioritize_ui_threads: false,
        }
    }
}

/// Write the `[system]` keys into a parsed `config.toml` document. The table
/// must already exist as an explicit table (the caller ensures it).
pub(crate) fn patch_document(doc: &mut DocumentMut, system: &SystemConfig) {
    doc["system"]["restore-last-session-when-opening"] =
        value(system.restore_last_session_when_opening);
    doc["system"]["manage-subprocess-job"] = value(system.manage_subprocess_job);
    doc["system"]["warn-before-terminating-shell"] =
        value(system.warn_before_terminating_shell.as_str());
    doc["system"]["confirm-before-closing-workspace"] =
        value(system.confirm_before_closing_workspace);
    doc["system"]["prioritize-ui-threads"] = value(system.prioritize_ui_threads);
}
