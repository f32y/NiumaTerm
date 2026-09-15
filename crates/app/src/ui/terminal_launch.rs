use std::process;

use app::terminal_tab::settings::TerminalSettings;
use app::terminal_tab::view::{TerminalLaunch, TerminalPane};
use gpui::{AppContext, Context, Entity};
use nmt_agent::agent_process;
use nmt_config::local_state::TabState;
#[cfg(windows)]
use nmt_remote_net::net_pty::terminal_session;
use nmt_terminal::session::TerminalSessionConfig;
use rust_i18n::t;
use tracing::warn;

use crate::ui::Shell;
use crate::ui::settings::AppSettings;

pub(super) fn spawn_pane(
    cx: &mut impl AppContext,
    id: u64,
    state: TabState,
    profile_name: String,
) -> Result<Entity<TerminalPane>, String> {
    let agent_route = agent_process().allocate_route();
    let environment_overrides = agent_process().environment_for(&agent_route);

    let (cursor_shape, manage_process_tree, improve_powershell_compatibility) =
        cx.read_global(|settings: &TerminalSettings, _| {
            (
                settings.cursor_shape,
                settings.manage_subprocess_job,
                settings.improve_powershell_compatibility,
            )
        });

    let config = TerminalSessionConfig {
        shell: state.shell.clone(),
        args: state.args.clone(),
        working_dir: state.cwd.clone(),
        starting_title: Some(profile_name.clone()),
        cursor_shape,
        environment_overrides,
        manage_process_tree,
        improve_powershell_compatibility,
        ..TerminalSessionConfig::default()
    };

    TerminalPane::spawn(
        cx,
        id,
        TerminalLaunch {
            config: config.with_shell_integration(),
            restorable: state,
            profile_name,
            agent_route,
        },
    )
}

/// Fill a launch's blank shell from the default profile and resolve the
/// display name of the profile it runs. A `None` shell means "follow the
/// default profile" (session persistence); resolving it here keeps the
/// hardcoded built-in fallback in the session layer from swallowing the
/// configured profile. The pane takes only the resolved values, so profile
/// policy stays with the settings that define it.
pub(super) fn launch_with_profile(
    tab_state: Option<TabState>,
    default_profile: (Option<String>, Vec<String>),
    cx: &mut impl AppContext,
) -> (TabState, String) {
    let mut tab_state = tab_state.unwrap_or_default();

    if tab_state.shell.is_none() {
        tab_state.shell = default_profile.0;
        tab_state.args = default_profile.1;
    }

    let profile_name = cx.read_global(|settings: &AppSettings, _| {
        settings.profile_name_for_command(tab_state.shell.as_deref(), &tab_state.args)
    });

    (tab_state, profile_name)
}

/// Spawn a pane on the default profile, starting the shell in `cwd` when
/// given. Falls back in layers: an unusable cwd retries without it, a
/// broken profile retries the built-in shell.
pub(super) fn spawn_default_pane(
    cx: &mut Context<Shell>,
    surface_id: u64,
    default_profile: (Option<String>, Vec<String>),
    cwd: Option<String>,
) -> Entity<TerminalPane> {
    let launch = cwd.map(|cwd| TabState {
        shell: default_profile.0.clone(),
        args: default_profile.1.clone(),
        cwd: Some(cwd),
        agent: None,
        ..TabState::default()
    });

    let (launch, profile_name) = launch_with_profile(launch, default_profile.clone(), cx);

    let spawned = spawn_pane(cx, surface_id, launch, profile_name).or_else(|error| {
        warn!("spawn with workspace cwd/profile failed, retrying default: {error}");

        let (launch, profile_name) = launch_with_profile(None, default_profile, cx);

        spawn_pane(cx, surface_id, launch, profile_name)
    });

    let pane = match spawned {
        Ok(pane) => pane,
        Err(error) => {
            warn!("default profile failed, retrying built-in shell: {error}");

            let (launch, profile_name) = launch_with_profile(None, (None, Vec::new()), cx);

            match spawn_pane(cx, surface_id, launch, profile_name) {
                Ok(pane) => pane,
                Err(error) => {
                    // Even the built-in shell cannot spawn (e.g. ConPTY
                    // unavailable) — no terminal can ever open, so tell
                    // the user why before exiting instead of dying with
                    // an invisible panic.
                    crate::show_startup_error_dialog(&t!(
                        "startup-terminal-spawn-error",
                        error = error
                    ));

                    process::exit(1);
                }
            }
        }
    };

    Shell::watch_pane(&pane, cx);

    pane
}

#[cfg(windows)]
pub(super) fn attach_remote(
    cx: &mut impl AppContext,
    id: u64,
    remote: nmt_remote_net::RemoteSession,
) -> Result<Entity<TerminalPane>, String> {
    let route = agent_process().allocate_route();

    TerminalPane::attach(
        cx,
        id,
        t!("terminal-remote-profile-name").to_string(),
        route,
        move |observer| terminal_session(remote, id, nmt_config::active_colors(), Some(observer)),
    )
}
