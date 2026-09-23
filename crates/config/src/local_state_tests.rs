use std::env;

use tempfile::tempdir;

use crate::local_state::*;

#[test]
fn save_load_roundtrip_and_legacy_file_defaults() {
    let dir = env::temp_dir().join("NiumaTerm-local-state-test");
    let _ = fs::remove_dir_all(&dir);
    let path = dir.join("local_state.toml");

    // Missing file: default state.
    assert_eq!(try_load_from(&path).unwrap(), LocalState::default());

    let state = LocalState {
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
                        tabs: vec![
                            TabState {
                                name: Some("editor".into()),
                                user_named: true,
                                shell: Some("pwsh.exe".into()),
                                args: vec!["-NoLogo".into()],
                                cwd: Some("C:/Projects/example/repo".into()),
                                agent: None,
                                agent_profile: None,
                                team_room: None,
                                git_cwd: None,
                                title: Some("vim notes.md".into()),
                                agent_conversation: None,
                                agent_settings: None,
                                panes: None,
                                grid_size: Some((132, 43)),
                            },
                            TabState {
                                agent: Some("claude".into()),
                                agent_profile: Some("reviewer".into()),
                                title: Some("Fix login redirect".into()),
                                agent_conversation: Some("session-7".into()),
                                agent_settings: Some(AgentTabSettings {
                                    model: Some("opus".into()),
                                    approval: Some("acceptEdits".into()),
                                    effort: Some("high".into()),
                                    ..AgentTabSettings::default()
                                }),
                                ..TabState::default()
                            },
                            TabState {
                                team_room: Some("40000000-0000-4000-8000-000000000000".into()),
                                ..TabState::default()
                            },
                        ],
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

    assert_eq!(try_load_from(&path).unwrap(), state);
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

    assert_eq!(try_load_from(&path).unwrap(), LocalState::default());

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
fn globally_stored_agent_defaults_are_ignored() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("local_state.toml");

    // Written by a build that kept one set of thread controls per profile
    // instead of per tab. The tabs now carry their own, so the old table is
    // read as an unknown key and dropped on the next save.
    let legacy = "[agent_defaults.claude]\nmodel = \"opus\"\n";

    fs::write(&path, legacy).unwrap();

    assert_eq!(try_load_from(&path).unwrap(), LocalState::default());

    save_windows_to(&path, &[]).unwrap();

    assert!(
        !fs::read_to_string(&path)
            .unwrap()
            .contains("agent_defaults")
    );
}

#[test]
fn local_state_read_errors_are_reported_and_preserved() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("local_state.toml");

    fs::write(&path, [0xff]).unwrap();

    assert_eq!(
        try_load_from(&path).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    assert!(save_windows_to(&path, &[]).is_err());
    assert_eq!(fs::read(&path).unwrap(), [0xff]);
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
        team_room: None,
        grid_size: Some((80, 36)),
        git_cwd: None,
        title: None,
        agent_conversation: None,
        agent_settings: None,
        panes: Some(PaneNodeState::Split {
            axis: PaneSplitAxis::Horizontal,
            ratios: vec![0.6, 0.4],
            children: vec![
                PaneNodeState::Leaf {
                    shell: Some("pwsh.exe".into()),
                    args: vec!["-NoLogo".into()],
                    cwd: Some("C:/a".into()),
                    grid_size: Some((80, 36)),
                },
                PaneNodeState::Split {
                    axis: PaneSplitAxis::Vertical,
                    ratios: vec![0.5, 0.5],
                    children: vec![
                        PaneNodeState::Leaf {
                            shell: None,
                            args: vec![],
                            cwd: Some("C:/b".into()),
                            grid_size: Some((53, 18)),
                        },
                        PaneNodeState::Leaf {
                            shell: None,
                            args: vec![],
                            cwd: None,
                            grid_size: None,
                        },
                    ],
                },
            ],
        }),
    };

    let state = LocalState {
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

    assert_eq!(try_load_from(&path).unwrap(), state);

    // A single-pane tab serializes without any `panes` key at all.
    let flat = LocalState {
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
    assert!(!fs::read_to_string(&path).unwrap().contains("grid_size"));

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

    let loaded = try_load_from(&path).unwrap();
    let tab = &loaded.windows[0].session.as_ref().unwrap().workspaces[0].tabs[0];

    assert_eq!(tab.name.as_deref(), Some("Tab 1"));
    assert_eq!(tab.panes, None);
    assert_eq!(tab.grid_size, None);
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

    let workspace = try_load_from(&path).unwrap().windows[0]
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
    assert_eq!(try_load_from(&path).unwrap(), single);

    // Ordered additions round-trip, and an older build that ignores the
    // key still restores the primary directory and the tabs.
    let mut multi = single.clone();

    multi.windows[0].session.as_mut().unwrap().workspaces[0].additional_cwds =
        vec!["C:/Projects/library".into(), "D:/Docs".into()];

    save_to(&path, &multi).unwrap();

    assert_eq!(try_load_from(&path).unwrap(), multi);

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

#[test]
fn git_tab_roundtrips_without_acquiring_a_shell_or_agent() {
    let git = TabState {
        git_cwd: Some("/project/nested".into()),
        ..TabState::default()
    };

    let serialized = toml::to_string(&git).unwrap();
    let restored: TabState = toml::from_str(&serialized).unwrap();

    assert_eq!(restored, git);
    assert!(restored.shell.is_none());
    assert!(restored.agent.is_none());

    let legacy: TabState = toml::from_str("cwd = '/project'\n").unwrap();

    assert!(legacy.git_cwd.is_none());
}
