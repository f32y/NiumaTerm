#![cfg(target_os = "windows")]
#![windows_subsystem = "windows"]

use std::ffi::OsString;
use std::rc::Rc;

use clap::{Arg, ArgAction, Command as ClapCommand};
use futures::StreamExt as _;
use gpui::{App, Application, KeyBinding};
use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};

mod cli;
mod error;
mod ipc;
mod logging;
mod pane_tree;
mod tabs;
mod terminal;
mod ui;
mod utils;
mod window;
mod workspace;

use nmt_config::local_state::LocalState;

use crate::cli::CliAction;
use crate::terminal::view::{
    CopyBlockCommand, CopyBlockOutput, NextBlock, PreviousBlock, RerunBlock, SendShiftTab, SendTab,
};
use crate::ui::{
    AppAssets, AppSettings, CloseTab, NewTab, NewWindow, NewWorkspace, NextTab, NextWorkspace,
    PrevTab, PrevWorkspace, ResizePaneDown, ResizePaneLeft, ResizePaneRight, ResizePaneUp,
    ShowSettings, SplitDown, SplitLeft, SplitRight, SplitUp, ToggleSidebar,
};
use crate::window::{AppWindow, LastActiveWindow, ShellRegistry, WindowRegistry};

enum StartupArgs {
    Run { url: Option<String>, testing: bool },
    RegisterShellExtension,
    UnregisterShellExtension,
}

struct StartupFiles {
    remembered_state: LocalState,
}

/// The concrete Windows platform, kept as a gpui global so settings toggles
/// can reach platform-level knobs (UI thread priority).
pub(crate) struct PlatformHandle(pub(crate) Rc<gpui_windows::WindowsPlatform>);

impl gpui::Global for PlatformHandle {}

fn main() {
    let startup_args = parse_startup_args();
    match startup_args {
        StartupArgs::RegisterShellExtension => {
            if let Err(err) = nmt_platform::register_shell_integration() {
                eprintln!("failed to register shell extension: {err:#}");
                std::process::exit(1);
            }
            return;
        }
        StartupArgs::UnregisterShellExtension => {
            if let Err(err) = nmt_platform::unregister_shell_integration() {
                eprintln!("failed to unregister shell extension: {err:#}");
                std::process::exit(1);
            }
            return;
        }
        StartupArgs::Run { url, testing } => run_app(url, testing),
    }
}

fn parse_startup_args() -> StartupArgs {
    parse_startup_args_from(std::env::args_os())
}

fn parse_startup_args_from<I, T>(args: I) -> StartupArgs
where
    I: IntoIterator<Item = T>,
    T: Into<OsString>,
{
    let args = args.into_iter().map(|arg| {
        let arg = arg.into();
        match arg.to_str() {
            Some("-registerShellExtension") => OsString::from("--registerShellExtension"),
            Some("-unregisterShellExtension") => OsString::from("--unregisterShellExtension"),
            _ => arg,
        }
    });

    let matches = ClapCommand::new("NiumaTerm")
        .disable_help_flag(true)
        .arg(
            Arg::new("register")
                .long("registerShellExtension")
                .action(ArgAction::SetTrue)
                .conflicts_with_all(["unregister", "new-tab", "new-window", "url"]),
        )
        .arg(
            Arg::new("unregister")
                .long("unregisterShellExtension")
                .action(ArgAction::SetTrue)
                .conflicts_with_all(["register", "new-tab", "new-window", "url"]),
        )
        .arg(
            Arg::new("testing")
                .long("testing")
                .action(ArgAction::SetTrue)
                .conflicts_with_all(["register", "unregister"]),
        )
        .arg(
            Arg::new("new-tab")
                .long("new-tab")
                .value_name("PATH")
                .conflicts_with_all(["register", "unregister", "new-window", "url"]),
        )
        .arg(
            Arg::new("new-window")
                .long("new-window")
                .value_name("PATH")
                .conflicts_with_all(["register", "unregister", "new-tab", "url"]),
        )
        .arg(Arg::new("url").index(1).conflicts_with_all([
            "register",
            "unregister",
            "new-tab",
            "new-window",
        ]))
        .try_get_matches_from(args)
        .unwrap_or_else(|err| {
            eprintln!("{err}");
            std::process::exit(2);
        });

    if matches.get_flag("register") {
        StartupArgs::RegisterShellExtension
    } else if matches.get_flag("unregister") {
        StartupArgs::UnregisterShellExtension
    } else {
        StartupArgs::Run {
            testing: matches.get_flag("testing"),
            url: matches
                .get_one::<String>("url")
                .cloned()
                .or_else(|| {
                    matches
                        .get_one::<String>("new-tab")
                        .map(|path| cli::path_action_url("new_tab", path))
                })
                .or_else(|| {
                    matches
                        .get_one::<String>("new-window")
                        .map(|path| cli::path_action_url("new_window", path))
                }),
        }
    }
}

fn run_app(argv_url: Option<String>, testing: bool) {
    nmt_agent_hook::agent_process().set_testing(testing);
    nmt_agent_hook::agent_process().set_hook_executable(
        utils::get_exe_dir()
            .join("NiumaTermHook.exe")
            .display()
            .to_string(),
    );
    if testing {
        nmt_config::enable_testing_mode();
    }
    // Hold the appender guard for the whole app lifetime; `main` blocks until exit.
    let _log_guard = crate::logging::init_logging().expect("init logging");
    let startup_files = load_startup_files_or_exit();
    // A second launch forwards its action to the existing process so one process
    // URL (or an activate request) to the running instance and exits. A
    // malformed URL degrades to activate — the primary just comes forward.
    let argv_action = argv_url.map(|url| {
        cli::parse_nmt_url(&url).unwrap_or_else(|err| {
            tracing::warn!("ignoring command line: {err}");
            CliAction::Activate
        })
    });
    if !nmt_platform::windows::ipc::try_become_primary(testing) {
        let action = argv_action.clone().unwrap_or(CliAction::Activate);
        match nmt_platform::windows::ipc::send(
            &action.to_url(),
            std::time::Duration::from_secs(2),
            testing,
        ) {
            Ok(()) => return,
            Err(error) => tracing::warn!("primary instance pipe unreachable: {error}"),
        }
        // The mutex holder never answered (booting forever, or hung): serve
        // the user with a fresh primary rather than doing nothing.
    }
    let (cli_tx, mut cli_rx) = futures::channel::mpsc::unbounded::<ipc::IpcAction>();
    ipc::spawn_pipe_server(cli_tx.clone(), testing);
    if let Some(action) = argv_action {
        // The primary's own argv URL joins the same dispatch path as
        // forwarded ones, applied after startup (and session restore).
        let _ = cli_tx.unbounded_send(ipc::IpcAction::Cli(action));
    }
    let platform = Rc::new(
        gpui_windows::WindowsPlatform::new(false).expect("failed to initialize GPUI Windows"),
    );
    let platform_handle = platform.clone();
    Application::with_platform(platform)
        // Serve project icons + gpui-component's embedded icons so `svg().path()`
        // resolves both.
        .with_assets(AppAssets)
        .run(move |cx: &mut App| {
            // Initialize gpui-component (theme, root, component globals) before any
            // component renders. Themes without `[colors.ui]` retain the dark default.
            gpui_component::init(cx);
            crate::ui::apply_ui_theme(nmt_config::get().ui_theme.as_ref(), cx);
            cx.set_global(AppSettings::load());
            crate::ui::apply_window_translucency(cx);
            nmt_platform::set_job_management(cx.global::<AppSettings>().manage_subprocess_job);
            // The platform remembers the choice and applies it to the vsync
            // thread when that spawns (after this closure returns).
            if cx.global::<AppSettings>().prioritize_ui_threads {
                platform_handle.set_ui_thread_priority(true);
            }
            cx.set_global(PlatformHandle(platform_handle));
            // Keep live behavior in sync on any settings change. Persistence is
            // deferred to when the settings dialog closes (see Shell::on_show_settings).
            cx.observe_global::<AppSettings>(|cx| {
                nmt_platform::set_job_management(cx.global::<AppSettings>().manage_subprocess_job);
                // Opacity changes retint the theme and switch each window
                // between acrylic composition and opaque presentation.
                crate::ui::apply_window_translucency(cx);
                let background = crate::ui::window_background_appearance(cx);
                let handles: Vec<_> = cx
                    .global::<ShellRegistry>()
                    .0
                    .iter()
                    .map(|entry| entry.handle)
                    .collect();
                for handle in handles {
                    handle
                        .update(cx, |_, window, _| {
                            window.set_background_appearance(background)
                        })
                        .ok();
                }
                cx.refresh_windows();
            })
            .detach();

            cx.bind_keys([
                KeyBinding::new("ctrl-shift-t", NewTab, Some("Shell")),
                KeyBinding::new("ctrl-shift-w", CloseTab, Some("Shell")),
                KeyBinding::new("ctrl-tab", NextTab, Some("Shell")),
                KeyBinding::new("ctrl-shift-tab", PrevTab, Some("Shell")),
                KeyBinding::new("ctrl-shift-n", NewWorkspace, Some("Shell")),
                KeyBinding::new("ctrl-alt-n", NewWindow, Some("Shell")),
                KeyBinding::new("ctrl-pagedown", NextWorkspace, Some("Shell")),
                KeyBinding::new("ctrl-pageup", PrevWorkspace, Some("Shell")),
                KeyBinding::new("ctrl-shift-b", ToggleSidebar, Some("Shell")),
                KeyBinding::new("ctrl-,", ShowSettings, Some("Shell")),
                // Split-pane creation and keyboard resize. These consume the
                // xterm `\x1b[1;7A..D` / `\x1b[1;4A..D` arrow sequences before
                // the terminal encodes them (accepted conflict, see the
                // terminal-split-panes change).
                KeyBinding::new("ctrl-alt-up", SplitUp, Some("Shell")),
                KeyBinding::new("ctrl-alt-down", SplitDown, Some("Shell")),
                KeyBinding::new("ctrl-alt-left", SplitLeft, Some("Shell")),
                KeyBinding::new("ctrl-alt-right", SplitRight, Some("Shell")),
                KeyBinding::new("alt-shift-up", ResizePaneUp, Some("Shell")),
                KeyBinding::new("alt-shift-down", ResizePaneDown, Some("Shell")),
                KeyBinding::new("alt-shift-left", ResizePaneLeft, Some("Shell")),
                KeyBinding::new("alt-shift-right", ResizePaneRight, Some("Shell")),
                // Tab/Shift-Tab go to the shell (completion) while the
                // terminal is focused. The deeper `Terminal` context wins over
                // `Root`'s tab → focus-traversal binding, which would
                // otherwise consume the key before the pane ever saw it.
                KeyBinding::new("tab", SendTab, Some("Terminal")),
                KeyBinding::new("shift-tab", SendShiftTab, Some("Terminal")),
                // Command-block navigation and actions on the selected block.
                KeyBinding::new("ctrl-shift-up", PreviousBlock, Some("Terminal")),
                KeyBinding::new("ctrl-shift-down", NextBlock, Some("Terminal")),
                KeyBinding::new("ctrl-shift-y", CopyBlockCommand, Some("Terminal")),
                KeyBinding::new("ctrl-shift-o", CopyBlockOutput, Some("Terminal")),
                KeyBinding::new("ctrl-shift-r", RerunBlock, Some("Terminal")),
            ]);

            // Restore local state; first run centers and starts one default tab.
            let remembered_state = startup_files.remembered_state.clone();
            let restore_session = cx.global::<AppSettings>().restore_last_session_when_opening;
            let mut initials: Vec<AppWindow> = if restore_session {
                remembered_state
                    .windows
                    .iter()
                    .map(|w| AppWindow::from_local_state(w, true))
                    .collect()
            } else {
                // Restore disabled: one window, first remembered geometry.
                let first = remembered_state
                    .windows
                    .first()
                    .cloned()
                    .unwrap_or_default();
                vec![AppWindow::from_local_state(&first, false)]
            };
            if initials.is_empty() {
                initials.push(AppWindow {
                    bounds: None,
                    session: None,
                    sidebar_width: None,
                    initial_cwd: None,
                });
            }
            // Restore disabled with saved sessions: rewrite the file without
            // them now, so a crash before quit can't resurrect them.
            if !restore_session && remembered_state.windows.iter().any(|w| w.session.is_some()) {
                let clean = LocalState {
                    windows: initials.iter().map(|w| w.to_local_state(false)).collect(),
                };
                if let Err(err) = nmt_config::local_state::save(&clean) {
                    tracing::warn!("failed to clear sessions from local_state.toml: {err}");
                }
            }
            cx.set_global(WindowRegistry(Vec::new()));
            cx.set_global(ShellRegistry(Vec::new()));
            cx.set_global(LastActiveWindow(None));
            // A closed window is discarded — except the last one: GPUI's
            // LastWindowClosed quit follows, and the quit hook saves it.
            cx.on_window_closed(|cx, window_id| {
                if !cx.windows().is_empty() {
                    cx.global_mut::<WindowRegistry>().remove(window_id);
                }
                cx.global_mut::<ShellRegistry>().remove(window_id);
                let last_active = cx.global_mut::<LastActiveWindow>();
                if last_active.0 == Some(window_id) {
                    last_active.0 = None;
                }
            })
            .detach();
            cx.on_app_quit(|cx| {
                let save_session = cx.global::<AppSettings>().restore_last_session_when_opening;
                let state = LocalState {
                    windows: cx
                        .global::<WindowRegistry>()
                        .0
                        .iter()
                        .map(|(_, w)| w.to_local_state(save_session))
                        .collect(),
                };
                if !state.windows.is_empty()
                    && let Err(err) = nmt_config::local_state::save(&state)
                {
                    tracing::warn!("failed to save local_state.toml: {err}");
                }
                async {}
            })
            .detach();

            for initial in initials {
                AppWindow::open(cx, initial);
            }
            // Apply CLI actions (argv + forwarded over the IPC pipe) on the
            // foreground; windows above exist before the first poll.
            cx.spawn(async move |cx| {
                while let Some(action) = cli_rx.next().await {
                    cx.update(|cx| match action {
                        ipc::IpcAction::Cli(action) => dispatch_cli_action(action, cx),
                        ipc::IpcAction::Agent(event) => dispatch_agent_event(event, cx),
                    });
                }
            })
            .detach();
            cx.activate(true);
        });
}

fn load_startup_files_or_exit() -> StartupFiles {
    let config = nmt_config::Config::load_for_startup().unwrap_or_else(|err| {
        startup_error_and_exit("config.toml", &err.to_string());
    });
    nmt_config::init(config);

    let remembered_state = nmt_config::local_state::try_load().unwrap_or_else(|err| {
        startup_error_and_exit("local_state.toml", &err.to_string());
    });

    StartupFiles { remembered_state }
}

fn startup_error_and_exit(file: &str, error: &str) -> ! {
    show_startup_error_dialog(&format!("Failed to parse {file}:\n\n{error}"));
    std::process::exit(1);
}

fn show_startup_error_dialog(message: &str) {
    let title: Vec<u16> = "NiumaTerm configuration error"
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let message: Vec<u16> = message.encode_utf16().chain(Some(0)).collect();
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            message.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_shell_extension_flags() {
        assert!(matches!(
            parse_startup_args_from(["NiumaTerm", "-registerShellExtension"]),
            StartupArgs::RegisterShellExtension
        ));
        assert!(matches!(
            parse_startup_args_from(["NiumaTerm", "-unregisterShellExtension"]),
            StartupArgs::UnregisterShellExtension
        ));
    }

    #[test]
    fn keeps_url_argument_for_normal_launch() {
        let StartupArgs::Run { url, .. } =
            parse_startup_args_from(["NiumaTerm", "nmt://action/new_tab?path=C%3A%2FWorkspace"])
        else {
            panic!("expected normal launch");
        };
        assert_eq!(
            url.as_deref(),
            Some("nmt://action/new_tab?path=C%3A%2FWorkspace")
        );
    }

    #[test]
    fn parses_shell_extension_path_flags() {
        let StartupArgs::Run { url, .. } =
            parse_startup_args_from(["NiumaTerm", "--new-tab", r"C:\A Dir"])
        else {
            panic!("expected normal launch");
        };
        assert_eq!(
            url.as_deref(),
            Some("nmt://action/new_tab?path=C%3A%5CA%20Dir")
        );

        let StartupArgs::Run { url, .. } =
            parse_startup_args_from(["NiumaTerm", "--new-window", r"C:\A&B"])
        else {
            panic!("expected normal launch");
        };
        assert_eq!(
            url.as_deref(),
            Some("nmt://action/new_window?path=C%3A%5CA%26B")
        );
    }

    #[test]
    fn parses_testing_mode() {
        let StartupArgs::Run { url, testing } = parse_startup_args_from(["NiumaTerm", "--testing"])
        else {
            panic!("expected testing launch");
        };
        assert!(testing);
        assert!(url.is_none());

        let StartupArgs::Run { testing, .. } = parse_startup_args_from(["NiumaTerm"]) else {
            panic!("expected normal launch");
        };
        assert!(!testing);
    }
}

/// The most recently active window's shell, falling back to the newest open
/// window when none was activated yet (or the active one just closed).
fn last_active_shell(
    cx: &App,
) -> Option<(gpui::AnyWindowHandle, gpui::WeakEntity<crate::ui::Shell>)> {
    let registry = cx.global::<ShellRegistry>();
    let last = cx.global::<LastActiveWindow>().0;
    registry
        .0
        .iter()
        .find(|entry| Some(entry.window_id) == last)
        .or_else(|| registry.0.last())
        .map(|entry| (entry.handle, entry.shell.clone()))
}

fn foreground_last_active(cx: &mut App) {
    if let Some((handle, _)) = last_active_shell(cx) {
        let _ = handle.update(cx, |_, window, _| window.activate_window());
    }
}

/// Apply one `nmt://` action: validate the target
/// directory, then open it as a tab in the best-matching workspace or as a
/// new window. Invalid targets only bring the app forward.
fn dispatch_cli_action(action: CliAction, cx: &mut App) {
    if let CliAction::FocusNotification {
        route,
        notification_id,
    } = action
    {
        dispatch_focus_notification(&route, &notification_id, cx);
        return;
    }
    let path = match &action {
        CliAction::Activate => {
            foreground_last_active(cx);
            return;
        }
        CliAction::NewTab { path } | CliAction::NewWindow { path } => {
            if !path.is_dir() {
                tracing::warn!("nmt:// target is not a directory: {}", path.display());
                foreground_last_active(cx);
                return;
            }
            path.clone()
        }
        CliAction::FocusNotification { .. } => unreachable!("handled above"),
    };
    match action {
        CliAction::NewTab { .. } => {
            // No live window (all closed mid-dispatch): degrade to new_window.
            let Some((handle, shell)) = last_active_shell(cx) else {
                open_window_at(&path, cx);
                return;
            };
            let opened = handle.update(cx, |_, window, cx| {
                let ok = shell
                    .update(cx, |shell, cx| shell.open_dir_tab(&path, window, cx))
                    .is_ok();
                if ok {
                    window.activate_window();
                }
                ok
            });
            if !matches!(opened, Ok(true)) {
                open_window_at(&path, cx);
            }
        }
        CliAction::NewWindow { .. } => open_window_at(&path, cx),
        CliAction::Activate => unreachable!("handled above"),
        CliAction::FocusNotification { .. } => unreachable!("handled above"),
    }
}

fn dispatch_focus_notification(
    route: &nmt_agent_hook::AgentRoute,
    notification_id: &str,
    cx: &mut App,
) {
    let targets: Vec<_> = cx
        .global::<ShellRegistry>()
        .0
        .iter()
        .map(|entry| (entry.handle, entry.shell.clone()))
        .collect();
    for (handle, shell) in targets {
        let focused = handle
            .update(cx, |_, window, cx| {
                shell
                    .update(cx, |shell, cx| {
                        shell.focus_notification(route, notification_id, window, cx)
                    })
                    .unwrap_or(false)
            })
            .unwrap_or(false);
        if focused {
            return;
        }
    }
    tracing::warn!("ignoring stale notification focus action");
}

fn dispatch_agent_event(event: nmt_agent_hook::AgentEvent, cx: &mut App) {
    let shells: Vec<_> = cx
        .global::<ShellRegistry>()
        .0
        .iter()
        .map(|entry| entry.shell.clone())
        .collect();
    for shell in shells {
        let event = event.clone();
        if shell
            .update(cx, |shell, cx| shell.apply_agent_event(event, cx))
            .unwrap_or(false)
        {
            return;
        }
    }
    tracing::warn!("ignoring agent event for unknown or closed route");
}

/// CLI `new_window`: a fresh window (default geometry) whose single
/// workspace is rooted at `path`.
fn open_window_at(path: &std::path::Path, cx: &mut App) {
    AppWindow::open(
        cx,
        AppWindow {
            bounds: None,
            session: None,
            sidebar_width: None,
            initial_cwd: Some(path.display().to_string()),
        },
    );
}
