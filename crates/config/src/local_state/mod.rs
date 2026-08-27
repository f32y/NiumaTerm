//! Machine-owned local state (window geometry), stored as
//! `local_state.toml` next to `config.toml`.
//!
//! Unlike `config.toml` this file is not meant for hand editing: it is
//! rewritten wholesale on save.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::{fs, io};

use serde::{Deserialize, Serialize};
use toml::de::Error as TomlError;
use toml::{from_str as parse_toml, to_string as serialize_toml};

use crate::config_dir_path;

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

/// A missing file loads as default; invalid TOML is reported to startup.
pub fn try_load() -> Result<LocalState, TomlError> {
    try_load_from(&local_state_file_path())
}

fn try_load_from(path: &Path) -> Result<LocalState, TomlError> {
    fs::read_to_string(path)
        .ok()
        .map_or(Ok(LocalState::default()), |content| parse_toml(&content))
}

/// Atomic write (temp file + rename).
pub fn save(state: &LocalState) -> io::Result<()> {
    save_to(&local_state_file_path(), state)
}

/// Replace the agent defaults without replacing window state that may have
/// been written by a different application instance.
pub fn save_agent_defaults(agent_defaults: &BTreeMap<String, AgentDefaults>) -> io::Result<()> {
    save_agent_defaults_to(&local_state_file_path(), agent_defaults)
}

fn save_agent_defaults_to(
    path: &Path,
    agent_defaults: &BTreeMap<String, AgentDefaults>,
) -> io::Result<()> {
    let mut state = match fs::read_to_string(path) {
        Ok(content) => parse_toml(&content)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))?,
        Err(err) if err.kind() == io::ErrorKind::NotFound => LocalState::default(),
        Err(err) => return Err(err),
    };
    state.agent_defaults.clone_from(agent_defaults);
    save_to(path, &state)
}

fn save_to(path: &Path, state: &LocalState) -> io::Result<()> {
    let content = serialize_toml(state)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("toml.tmp");
    fs::write(&tmp, content)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests;
