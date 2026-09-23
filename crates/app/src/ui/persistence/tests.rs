use gpui::TestAppContext;
use nmt_config::Config;
use nmt_config::local_state::TabState;
use nmt_config::profile::ProfilesConfig;

use crate::ui::persistence::{
    legacy_generated_tab_title, resolve_restored_launch, restore_tabs, restore_team_tab,
    session_state,
};
use crate::ui::settings::{AppSettings, Profile};
use crate::ui::shell::TabSurface;
use crate::workspace::{WorkspaceId, WorkspaceManager, WorkspaceRoots};

fn settings_with_pwsh_default() -> AppSettings {
    AppSettings::from_config(Config {
        profiles: ProfilesConfig {
            list: vec![
                Profile {
                    name: "PowerShell".into(),
                    shell: r"C:\Program Files\PowerShell\7\pwsh.exe".into(),
                    args: String::new(),
                },
                Profile {
                    name: "WSL".into(),
                    shell: "wsl.exe".into(),
                    args: "-d Ubuntu".into(),
                },
            ],
            default: "PowerShell".into(),
        },
        ..Config::default()
    })
}

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

#[gpui::test]
fn restored_git_tab_keeps_its_directory_without_resolving_a_shell(cx: &mut TestAppContext) {
    cx.update(|cx| {
        cx.set_global(settings_with_pwsh_default());

        let saved = TabState {
            git_cwd: Some("/project".into()),
            ..TabState::default()
        };

        let tabs = restore_tabs(vec![saved.clone()], 0, &mut 0, cx).unwrap();

        assert!(tabs.active().is_git());
        assert!(tabs.active().tree().is_none());
        assert_eq!(
            tabs.list().items()[0].title(),
            rust_i18n::t!("git-tab-title")
        );

        let TabSurface::Pending(restored) = tabs.active() else {
            panic!("restored tab should start pending");
        };

        assert_eq!(restored.as_ref(), &saved);
    });
}

#[gpui::test]
fn restored_team_tabs_keep_their_title(cx: &mut TestAppContext) {
    cx.update(|cx| {
        cx.set_global(settings_with_pwsh_default());

        let saved = TabState {
            team_room: Some("saved-room".into()),
            ..TabState::default()
        };

        let named = TabState {
            name: Some("Review team".into()),
            user_named: true,
            ..saved.clone()
        };

        let tabs = restore_tabs(vec![saved, named], 0, &mut 0, cx).unwrap();

        assert_eq!(tabs.list().items()[0].title(), rust_i18n::t!("team-title"));
        assert_eq!(tabs.list().items()[1].title(), "Review team");
    });
}

#[gpui::test]
fn restored_tabs_show_their_saved_title_across_launches(cx: &mut TestAppContext) {
    cx.update(|cx| {
        cx.set_global(settings_with_pwsh_default());

        let titled = TabState {
            title: Some("vim notes.md".into()),
            ..tab(None, &[])
        };

        let named = TabState {
            name: Some("editor".into()),
            user_named: true,
            ..titled.clone()
        };

        let tabs = restore_tabs(vec![titled, named, tab(None, &[])], 0, &mut 0, cx).unwrap();

        let shown: Vec<_> = tabs.list().items().iter().map(|tab| tab.title()).collect();

        assert_eq!(shown, ["vim notes.md", "editor", "PowerShell"]);

        // Tabs the user never activated save again on quit; losing the title
        // there would leave them unlabeled from the second launch on.
        let workspaces = WorkspaceManager::new(
            tabs,
            WorkspaceId(100),
            "Workspace".into(),
            WorkspaceRoots::single("C:/Projects".into()),
        );

        let saved = session_state(&workspaces, None, cx);

        let titles: Vec<_> = saved.workspaces[0]
            .tabs
            .iter()
            .map(|tab| tab.title.as_deref())
            .collect();

        assert_eq!(titles, [Some("vim notes.md"), Some("vim notes.md"), None]);
    });
}

#[gpui::test]
fn disabled_agent_team_restore_keeps_saved_state_without_opening_it(cx: &mut TestAppContext) {
    cx.set_global(settings_with_pwsh_default());

    let cx = cx.add_empty_window();

    cx.update(|window, cx| {
        let saved = TabState {
            team_room: Some("invalid-room-id".into()),
            name: Some("Review team".into()),
            user_named: true,
            ..TabState::default()
        };

        for enabled in [false, true, false] {
            cx.global_mut::<AppSettings>()
                .edit_agent(|agent| agent.enable_agent_team = enabled);

            let surface = restore_team_tab("invalid-room-id", &saved, window, cx);

            let restored = match surface {
                TabSurface::TeamDisabled(restored) if !enabled => restored,
                TabSurface::TeamUnavailable { saved, .. } if enabled => saved,
                _ => panic!("team restore did not honor the current setting"),
            };

            assert_eq!(*restored, saved);
        }
    });
}

#[test]
fn recognizes_generated_titles_from_legacy_snapshots() {
    assert!(legacy_generated_tab_title("Tab 1"));
    assert!(legacy_generated_tab_title("Tab 42"));
    assert!(!legacy_generated_tab_title("Tab"));
    assert!(!legacy_generated_tab_title("editor"));
}

#[test]
fn restore_resolves_none_to_default_profile() {
    let settings = settings_with_pwsh_default();

    let mut state = tab(None, &[]);

    resolve_restored_launch(&mut state, &settings);

    assert_eq!(
        state.shell.as_deref(),
        Some(r"C:\Program Files\PowerShell\7\pwsh.exe")
    );
    assert!(state.args.is_empty());
}

#[test]
fn restore_replaces_stale_shell_with_default_profile() {
    // The former built-in default is no longer in the profile list: the saved
    // pin is stale and must follow the current default profile instead.
    let settings = settings_with_pwsh_default();

    let mut state = tab(
        Some(r"C:\WINDOWS\System32\WindowsPowerShell\v1.0\powershell.exe"),
        &[],
    );

    resolve_restored_launch(&mut state, &settings);

    assert_eq!(
        state.shell.as_deref(),
        Some(r"C:\Program Files\PowerShell\7\pwsh.exe")
    );
}

#[test]
fn restore_keeps_shell_still_present_in_profiles() {
    let settings = settings_with_pwsh_default();

    let mut state = tab(Some("WSL.EXE"), &["-d", "Ubuntu"]);

    resolve_restored_launch(&mut state, &settings);

    assert_eq!(state.shell.as_deref(), Some("WSL.EXE"));
    assert_eq!(state.args, vec!["-d".to_string(), "Ubuntu".to_string()]);
}
