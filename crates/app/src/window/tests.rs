use nmt_config::local_state::{SessionState, TabState, WorkspaceState};

use crate::window::*;

fn window_state() -> WindowState {
    WindowState {
        x: 1.0,
        y: 2.0,
        width: 800.0,
        height: 600.0,
        maximized: false,
    }
}

fn session_state() -> SessionState {
    SessionState {
        active_workspace: 0,
        workspaces: vec![WorkspaceState {
            name: "Workspace 1".into(),
            cwd: Some("C:/Projects/example".into()),
            additional_cwds: Vec::new(),
            pinned: false,
            active_tab: 0,
            tabs: vec![TabState {
                name: None,
                user_named: false,
                shell: Some("pwsh.exe".into()),
                args: vec!["-NoLogo".into()],
                cwd: Some("C:/Projects/example/repo".into()),
                agent: None,
                agent_profile: None,
                panes: None,
            }],
        }],
    }
}

fn remembered() -> WindowLocalState {
    WindowLocalState {
        window: Some(window_state()),
        session: Some(session_state()),
        sidebar_width: Some(220.0),
    }
}

#[test]
fn restore_enabled_loads_session_and_quit_saves_both() {
    let state = AppWindow::from_local_state(&remembered(), true);
    assert_eq!(state.bounds, Some(window_state()));
    assert_eq!(state.session, Some(session_state()));
    assert_eq!(state.to_local_state(true), remembered());
}

#[test]
fn restore_disabled_discards_session_and_quit_skips_session_save() {
    let state = AppWindow::from_local_state(&remembered(), false);
    assert_eq!(state.bounds, Some(window_state()));
    assert_eq!(state.session, None);
    // The startup-cleanup save and the quit save both go through
    // `to_local_state(false)`: geometry kept, session cleared.
    let state = AppWindow {
        session: Some(session_state()),
        ..state
    };
    assert_eq!(
        state.to_local_state(false),
        WindowLocalState {
            window: Some(window_state()),
            session: None,
            sidebar_width: Some(220.0),
        }
    );
}
