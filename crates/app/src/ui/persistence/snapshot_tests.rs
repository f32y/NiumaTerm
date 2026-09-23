use nmt_config::local_state::TabState;

use crate::ui::persistence::snapshot::normalize_saved_launch;

fn tab(shell: Option<&str>, args: &[&str]) -> TabState {
    TabState {
        name: None,
        user_named: false,
        shell: shell.map(str::to_string),
        args: args.iter().map(|a| a.to_string()).collect(),
        cwd: None,
        agent: None,
        agent_profile: None,
        team_room: None,
        git_cwd: None,
        title: None,
        agent_conversation: None,
        agent_settings: None,
        panes: None,
        grid_size: None,
    }
}

#[test]
fn default_profile_pane_saves_as_follow_default() {
    let default = (Some("pwsh.exe".to_string()), vec!["-NoLogo".to_string()]);

    let mut state = tab(Some("pwsh.exe"), &["-NoLogo"]);

    normalize_saved_launch(&mut state, &default);

    assert_eq!(state.shell, None);
    assert!(state.args.is_empty());
}

#[test]
fn pinned_pane_keeps_its_saved_command() {
    let default = (Some("pwsh.exe".to_string()), Vec::new());

    let mut state = tab(Some("wsl.exe"), &["-d", "Ubuntu"]);

    normalize_saved_launch(&mut state, &default);

    assert_eq!(state.shell.as_deref(), Some("wsl.exe"));
    assert_eq!(state.args, vec!["-d".to_string(), "Ubuntu".to_string()]);
}
