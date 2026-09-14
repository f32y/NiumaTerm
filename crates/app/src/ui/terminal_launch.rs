use app::terminal_tab::settings::TerminalSettings;
use app::terminal_tab::view::{TerminalLaunch, TerminalPane};
use gpui::{AppContext, Entity};
use nmt_agent::agent_process;
use nmt_config::local_state::TabState;
use nmt_remote_net::net_pty::terminal_session;
use nmt_terminal::session::TerminalSessionConfig;
use rust_i18n::t;

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
