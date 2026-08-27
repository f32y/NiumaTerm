use std::env;

use crate::local_state::*;

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
