//! Machine-owned local state (window geometry), stored as
//! `local_state.toml` next to `config.toml`.
//!
//! Unlike `config.toml` this file is not meant for hand editing: it is
//! rewritten wholesale on save.

#[cfg(test)]
#[path = "local_state_tests.rs"]
mod local_state_tests;

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
}

/// The thread-settings picks one agent tab is running under, carried into the
/// conversations that tab opens later. All optional: `None` leaves the value
/// the launch profile and the CLI resolve between them.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentTabSettings {
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_preset: Option<String>,
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
    /// instead of a terminal. Restore reopens an agent tab of the same kind,
    /// and an unknown kind degrades to a plain terminal tab.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,

    /// Name of the agent launch profile the tab was opened with. Restore
    /// resolves it against the configured agent profiles; a missing or
    /// deleted name falls back to the built-in profile for `agent`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_profile: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_room: Option<String>,

    /// Last terminal grid, in columns and rows. Restoring the grid before
    /// spawning the shell avoids a resize during its initial prompt rendering.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grid_size: Option<(u16, u16)>,

    /// Directory reviewed by a Git tab. Older snapshots omit this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_cwd: Option<String>,

    /// The title the tab's content last reported: the shell's OSC title or
    /// the agent conversation's name. A restored tab shows it before it is
    /// activated, so tabs that have not spawned yet stay distinguishable
    /// instead of all carrying their profile name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,

    /// Provider id of the conversation an agent tab held. The harness keeps
    /// the conversation on disk, so a restored tab continues it; absent for a
    /// tab whose conversation never received a message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_conversation: Option<String>,

    /// Thread controls this agent tab was last running under. Absent for a
    /// tab the user never adjusted, which reopens on its profile's defaults.
    /// Declared with `panes` below the scalars: TOML requires tables after
    /// plain values.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_settings: Option<AgentTabSettings>,

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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        grid_size: Option<(u16, u16)>,
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

/// Save window state. The read-modify-replace cycle runs under the lock in
/// `persistence::update`, and a file that fails to decode is left untouched
/// rather than overwritten with what this instance happens to hold.
pub fn save_windows(windows: &[WindowLocalState]) -> io::Result<()> {
    save_windows_to(&local_state_file_path(), windows)
}

fn save_windows_to(path: &Path, windows: &[WindowLocalState]) -> io::Result<()> {
    persistence::update(path, |content| {
        let mut state = decode(content)?;

        state.windows = windows.to_vec();

        serialize_toml(&state).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    })
}

#[cfg(test)]
fn save_to(path: &Path, state: &LocalState) -> io::Result<()> {
    persistence::update(path, |_| {
        serialize_toml(state).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    })
}
