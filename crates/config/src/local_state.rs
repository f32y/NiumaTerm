//! Machine-owned local state (window geometry), stored as
//! `local_state.toml` next to `config.toml`.
//!
//! Unlike `config.toml` this file is not meant for hand editing: it is
//! rewritten wholesale on save.

use std::collections::BTreeMap;
#[cfg(test)]
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use toml::{from_str as parse_toml, to_string as serialize_toml};

use crate::{config_dir_path, persistence};

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LocalState {
    #[serde(default)]
    pub windows: Vec<WindowLocalState>,

    /// Last-chosen agent thread settings per agent profile name (older
    /// snapshots keyed by agent ID, which still reads as a fallback);
    /// newly opened agent tabs seed their dropdowns from these.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub agent_defaults: BTreeMap<String, AgentDefaults>,
}

/// The thread-settings picks worth carrying into the next conversation from
/// the same agent profile. All optional: `None` leaves the CLI's own default.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentDefaults {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approvals_reviewer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
}

/// One window's persisted state: geometry plus its session snapshot.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WindowLocalState {
    #[serde(default)]
    pub window: Option<WindowState>,
    #[serde(default)]
    pub session: Option<SessionState>,

    /// Expanded workspace-sidebar width in logical pixels.
    #[serde(default)]
    pub sidebar_width: Option<f32>,
}

/// Last-known window geometry (logical pixels, global coordinates).
/// When `maximized`, x/y/width/height hold the restore bounds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowState {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    #[serde(default)]
    pub maximized: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionState {
    #[serde(default)]
    pub active_workspace: usize,
    #[serde(default)]
    pub workspaces: Vec<WorkspaceState>,
}

impl SessionState {
    pub fn active_workspace_index(&self) -> Option<usize> {
        if self.workspaces.is_empty() {
            None
        } else {
            Some(self.active_workspace.min(self.workspaces.len() - 1))
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceState {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub cwd: Option<String>,

    /// Directories the workspace owns beyond its primary `cwd`, in workspace
    /// order. Defaulting when absent lets a snapshot written before
    /// multi-directory workspaces restore as a single-directory workspace, and
    /// omitting an empty list keeps those snapshots byte-identical. TOML
    /// requires every scalar field ahead of the `tabs` array of tables.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub additional_cwds: Vec<String>,

    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub active_tab: usize,
    #[serde(default)]
    pub tabs: Vec<TabState>,
}

impl WorkspaceState {
    pub fn active_tab_index(&self) -> Option<usize> {
        if self.tabs.is_empty() {
            None
        } else {
            Some(self.active_tab.min(self.tabs.len() - 1))
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TabState {
    /// User-authored display name shown in the tab bar.
    #[serde(default)]
    pub name: Option<String>,

    /// Distinguishes explicit names from older snapshots that persisted generated
    /// `Tab N` labels in `name`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub user_named: bool,

    #[serde(default)]
    pub shell: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub cwd: Option<String>,

    /// The agent kind ("codex") when this tab hosts an agent conversation
    /// instead of a terminal. Conversations are not persisted; restore
    /// reopens a fresh agent tab of the same kind, and an unknown kind
    /// degrades to a plain terminal tab.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,

    /// Name of the agent launch profile the tab was opened with. Restore
    /// resolves it against the configured agent profiles; a missing or
    /// deleted name falls back to the built-in profile for `agent`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_profile: Option<String>,

    /// Split-pane layout for a multi-pane tab. Absent for single-pane tabs,
    /// which keep the flat fields above as their whole format (so snapshots
    /// without splits stay readable by older builds). Declared last: TOML
    /// requires tables after plain values.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub panes: Option<PaneNodeState>,
}

/// One node of a tab's saved split-pane layout.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PaneNodeState {
    #[serde(rename = "leaf")]
    Leaf {
        #[serde(default)]
        shell: Option<String>,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        cwd: Option<String>,
    },

    #[serde(rename = "split")]
    Split {
        axis: PaneSplitAxis,

        /// Normalized child sizes (sum ≈ 1). Restore falls back to an equal
        /// split when absent or when the length mismatches `children`.
        #[serde(default)]
        ratios: Vec<f32>,

        children: Vec<PaneNodeState>,
    },
}

/// Split orientation: `h` = children side by side, `v` = children stacked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PaneSplitAxis {
    #[serde(rename = "h")]
    Horizontal,
    #[serde(rename = "v")]
    Vertical,
}

pub fn local_state_file_path() -> PathBuf {
    config_dir_path().join("local_state.toml")
}

/// A missing or invalid file loads as the default (empty) state.
pub fn load() -> LocalState {
    load_from(&local_state_file_path())
}

fn load_from(path: &Path) -> LocalState {
    try_load_from(path).unwrap_or_default()
}

/// A missing file loads as default; read and decoding failures reach startup.
pub fn try_load() -> io::Result<LocalState> {
    try_load_from(&local_state_file_path())
}

fn try_load_from(path: &Path) -> io::Result<LocalState> {
    decode(persistence::read(path)?.as_deref())
}

fn decode(content: Option<&str>) -> io::Result<LocalState> {
    content.map_or_else(
        || Ok(LocalState::default()),
        |content| {
            parse_toml(content).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
        },
    )
}

/// Atomic write (temp file + rename).
pub fn save(state: &LocalState) -> io::Result<()> {
    save_to(&local_state_file_path(), state)
}

/// Update only the supplied profiles, preserving windows and other profiles
/// that may have been written by another application instance.
pub fn save_agent_defaults(agent_defaults: &BTreeMap<String, AgentDefaults>) -> io::Result<()> {
    save_agent_defaults_to(&local_state_file_path(), agent_defaults)
}

fn save_agent_defaults_to(
    path: &Path,
    agent_defaults: &BTreeMap<String, AgentDefaults>,
) -> io::Result<()> {
    update_state(path, |state| {
        state.agent_defaults.extend(agent_defaults.clone());
    })
}

/// Save window state without replacing newer profile choices on disk.
pub fn save_windows(windows: &[WindowLocalState]) -> io::Result<()> {
    save_windows_to(&local_state_file_path(), windows)
}

fn save_windows_to(path: &Path, windows: &[WindowLocalState]) -> io::Result<()> {
    update_state(path, |state| state.windows = windows.to_vec())
}

fn update_state(path: &Path, edit: impl FnOnce(&mut LocalState)) -> io::Result<()> {
    persistence::update(path, |content| {
        let mut state = decode(content)?;

        edit(&mut state);

        serialize_toml(&state).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    })
}

fn save_to(path: &Path, state: &LocalState) -> io::Result<()> {
    persistence::update(path, |_| {
        serialize_toml(state).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    })
}

#[cfg(test)]
#[path = "local_state_tests.rs"]
mod local_state_tests;
