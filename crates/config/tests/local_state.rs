use nmt_config::local_state::{SessionState, TabState, WorkspaceState};

#[test]
fn session_indexes_clamp_to_existing_entries() {
    let state = SessionState {
        active_workspace: 99,
        workspaces: vec![WorkspaceState {
            name: "Workspace 1".into(),
            cwd: None,
            additional_cwds: Vec::new(),
            pinned: false,
            active_tab: 99,
            tabs: vec![
                TabState::default(),
                TabState {
                    shell: Some("cmd.exe".into()),
                    ..TabState::default()
                },
            ],
        }],
    };

    assert_eq!(state.active_workspace_index(), Some(0));
    assert_eq!(state.workspaces[0].active_tab_index(), Some(1));
    assert_eq!(SessionState::default().active_workspace_index(), None);
    assert_eq!(WorkspaceState::default().active_tab_index(), None);
}
