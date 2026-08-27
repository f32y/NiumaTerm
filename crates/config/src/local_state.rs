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
mod tests {
    use std::env;

    use super::*;

    #[test]
    fn save_load_roundtrip_and_bad_file_defaults() {
        let dir = env::temp_dir().join("NiumaTerm-local-state-test");
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("local_state.toml");

        // Missing file: default state.
        assert_eq!(load_from(&path), LocalState::default());

        let state = LocalState {
            agent_defaults: BTreeMap::from([(
                "claude".to_string(),
                AgentDefaults {
                    model: Some("opus".to_string()),
                    approval: Some("acceptEdits".to_string()),
                    approvals_reviewer: None,
                    sandbox: None,
                    effort: Some("high".to_string()),
                    tier: None,
                },
            )]),
            windows: vec![
                WindowLocalState {
                    window: Some(WindowState {
                        x: -8.0,
                        y: 42.5,
                        width: 960.0,
                        height: 620.0,
                        maximized: true,
                    }),
                    session: Some(SessionState {
                        active_workspace: 5,
                        workspaces: vec![WorkspaceState {
                            name: "Workspace 1".into(),
                            cwd: Some("C:/Projects/example".into()),
                            additional_cwds: vec!["C:/Projects/library".into(), "D:/Docs".into()],
                            pinned: true,
                            active_tab: 9,
                            tabs: vec![TabState {
                                name: Some("editor".into()),
                                user_named: true,
                                shell: Some("pwsh.exe".into()),
                                args: vec!["-NoLogo".into()],
                                cwd: Some("C:/Projects/example/repo".into()),
                                agent: None,
                                agent_profile: None,
                                panes: None,
                            }],
                        }],
                    }),
                    sidebar_width: Some(220.0),
                },
                WindowLocalState {
                    window: Some(WindowState {
                        x: 40.0,
                        y: 60.0,
                        width: 800.0,
                        height: 500.0,
                        maximized: false,
                    }),
                    session: None,
                    sidebar_width: None,
                },
            ],
        };
        save_to(&path, &state).unwrap();
        assert_eq!(load_from(&path), state);
        assert!(fs::read_to_string(&path).unwrap().contains("pinned = true"));
        assert!(
            fs::read_to_string(&path)
                .unwrap()
                .contains("user_named = true")
        );
        assert!(!path.with_extension("toml.tmp").exists());

        // Legacy single-window format: no `windows` list, loads as default.
        fs::write(
            &path,
            "window = { x = 1.0, y = 2.0, width = 3.0, height = 4.0 }",
        )
        .unwrap();
        assert_eq!(load_from(&path), LocalState::default());

        // Corrupt file: default state instead of an error.
        fs::write(&path, "not [ valid").unwrap();
        assert_eq!(load_from(&path), LocalState::default());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn try_load_defaults_when_missing_and_errors_on_bad_toml() {
        let dir = env::temp_dir().join("NiumaTerm-local-state-try-load-test");
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("local_state.toml");

        assert_eq!(try_load_from(&path).unwrap(), LocalState::default());

        fs::create_dir_all(&dir).unwrap();
        fs::write(&path, "not [ valid").unwrap();
        assert!(try_load_from(&path).is_err());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_agent_defaults_updates_only_agent_defaults() {
        let dir = env::temp_dir().join("NiumaTerm-local-state-agent-defaults-test");
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("local_state.toml");
        let initial = LocalState {
            windows: vec![WindowLocalState {
                window: Some(WindowState {
                    x: 10.0,
                    y: 20.0,
                    width: 900.0,
                    height: 600.0,
                    maximized: false,
                }),
                session: None,
                sidebar_width: Some(240.0),
            }],
            agent_defaults: BTreeMap::from([(
                "claude".to_string(),
                AgentDefaults {
                    model: Some("sonnet".to_string()),
                    ..AgentDefaults::default()
                },
            )]),
        };
        save_to(&path, &initial).unwrap();

        let defaults = BTreeMap::from([
            (
                "claude".to_string(),
                AgentDefaults {
                    model: Some("opus".to_string()),
                    approval: Some("acceptEdits".to_string()),
                    ..AgentDefaults::default()
                },
            ),
            (
                "codex".to_string(),
                AgentDefaults {
                    model: Some("gpt-5.6-codex".to_string()),
                    approvals_reviewer: Some("auto_review".to_string()),
                    effort: Some("high".to_string()),
                    ..AgentDefaults::default()
                },
            ),
        ]);

        save_agent_defaults_to(&path, &defaults).unwrap();

        let saved = try_load_from(&path).unwrap();
        assert_eq!(saved.windows, initial.windows);
        assert_eq!(saved.agent_defaults, defaults);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn pane_layout_roundtrips_and_old_snapshots_load_without_it() {
        let dir = env::temp_dir().join("NiumaTerm-pane-layout-test");
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("local_state.toml");

        // A split tab: h[ leaf, v[leaf, leaf] ] with saved ratios.
        let split_tab = TabState {
            name: Some("Tab 1".into()),
            user_named: false,
            shell: Some("pwsh.exe".into()),
            args: vec![],
            cwd: Some("C:/a".into()),
            agent: None,
            agent_profile: None,
            panes: Some(PaneNodeState::Split {
                axis: PaneSplitAxis::Horizontal,
                ratios: vec![0.6, 0.4],
                children: vec![
                    PaneNodeState::Leaf {
                        shell: Some("pwsh.exe".into()),
                        args: vec!["-NoLogo".into()],
                        cwd: Some("C:/a".into()),
                    },
                    PaneNodeState::Split {
                        axis: PaneSplitAxis::Vertical,
                        ratios: vec![0.5, 0.5],
                        children: vec![
                            PaneNodeState::Leaf {
                                shell: None,
                                args: vec![],
                                cwd: Some("C:/b".into()),
                            },
                            PaneNodeState::Leaf {
                                shell: None,
                                args: vec![],
                                cwd: None,
                            },
                        ],
                    },
                ],
            }),
        };
        let state = LocalState {
            agent_defaults: Default::default(),
            windows: vec![WindowLocalState {
                window: None,
                session: Some(SessionState {
                    active_workspace: 0,
                    workspaces: vec![WorkspaceState {
                        name: "Workspace 1".into(),
                        cwd: None,
                        additional_cwds: Vec::new(),
                        pinned: false,
                        active_tab: 0,
                        tabs: vec![split_tab],
                    }],
                }),
                sidebar_width: None,
            }],
        };
        save_to(&path, &state).unwrap();
        assert_eq!(load_from(&path), state);

        // A single-pane tab serializes without any `panes` key at all.
        let flat = LocalState {
            agent_defaults: Default::default(),
            windows: vec![WindowLocalState {
                window: None,
                session: Some(SessionState {
                    active_workspace: 0,
                    workspaces: vec![WorkspaceState {
                        name: "Workspace 1".into(),
                        cwd: None,
                        additional_cwds: Vec::new(),
                        pinned: false,
                        active_tab: 0,
                        tabs: vec![TabState::default()],
                    }],
                }),
                sidebar_width: None,
            }],
        };
        save_to(&path, &flat).unwrap();
        assert!(!fs::read_to_string(&path).unwrap().contains("panes"));

        // A pre-pane-layout snapshot (no `panes` key) loads with `panes: None`.
        fs::write(
            &path,
            r#"
[[windows]]
[windows.session]
active_workspace = 0
[[windows.session.workspaces]]
name = "Workspace 1"
active_tab = 0
[[windows.session.workspaces.tabs]]
name = "Tab 1"
shell = "pwsh.exe"
"#,
        )
        .unwrap();
        let loaded = load_from(&path);
        let tab = &loaded.windows[0].session.as_ref().unwrap().workspaces[0].tabs[0];
        assert_eq!(tab.name.as_deref(), Some("Tab 1"));
        assert_eq!(tab.panes, None);
        assert!(!loaded.windows[0].session.as_ref().unwrap().workspaces[0].pinned);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn additional_workspace_directories_survive_older_and_newer_snapshots() {
        let dir = env::temp_dir().join("NiumaTerm-additional-cwds-test");
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("local_state.toml");
        fs::create_dir_all(&dir).unwrap();

        // A snapshot written before multi-directory workspaces existed.
        fs::write(
            &path,
            r#"
[[windows]]
[windows.session]
active_workspace = 0
[[windows.session.workspaces]]
name = "Workspace 1"
cwd = "C:/Projects/example"
active_tab = 0
"#,
        )
        .unwrap();
        let workspace = load_from(&path).windows[0]
            .session
            .clone()
            .unwrap()
            .workspaces
            .remove(0);
        assert_eq!(workspace.cwd.as_deref(), Some("C:/Projects/example"));
        assert!(workspace.additional_cwds.is_empty());

        // A workspace without additions writes no key at all, so an older
        // build reads back exactly what it wrote.
        let single = LocalState {
            agent_defaults: Default::default(),
            windows: vec![WindowLocalState {
                window: None,
                session: Some(SessionState {
                    active_workspace: 0,
                    workspaces: vec![WorkspaceState {
                        name: "Workspace 1".into(),
                        cwd: Some("C:/Projects/example".into()),
                        additional_cwds: Vec::new(),
                        pinned: false,
                        active_tab: 0,
                        tabs: vec![TabState::default()],
                    }],
                }),
                sidebar_width: None,
            }],
        };
        save_to(&path, &single).unwrap();
        assert!(
            !fs::read_to_string(&path)
                .unwrap()
                .contains("additional_cwds")
        );
        assert_eq!(load_from(&path), single);

        // Ordered additions round-trip, and an older build that ignores the
        // key still restores the primary directory and the tabs.
        let mut multi = single.clone();
        multi.windows[0].session.as_mut().unwrap().workspaces[0].additional_cwds =
            vec!["C:/Projects/library".into(), "D:/Docs".into()];
        save_to(&path, &multi).unwrap();
        assert_eq!(load_from(&path), multi);

        #[derive(Debug, Deserialize)]
        struct LegacyWorkspace {
            cwd: Option<String>,
            #[serde(default)]
            tabs: Vec<TabState>,
        }
        #[derive(Debug, Deserialize)]
        struct LegacySession {
            workspaces: Vec<LegacyWorkspace>,
        }
        #[derive(Debug, Deserialize)]
        struct LegacyWindow {
            session: LegacySession,
        }
        #[derive(Debug, Deserialize)]
        struct LegacyState {
            windows: Vec<LegacyWindow>,
        }
        let legacy: LegacyState =
            toml::from_str(&fs::read_to_string(&path).unwrap()).expect("older build parses");
        let workspace = &legacy.windows[0].session.workspaces[0];
        assert_eq!(workspace.cwd.as_deref(), Some("C:/Projects/example"));
        assert_eq!(workspace.tabs.len(), 1);

        let _ = fs::remove_dir_all(&dir);
    }
}
