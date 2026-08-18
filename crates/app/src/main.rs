#![cfg(target_os = "windows")]
#![windows_subsystem = "windows"]

use std::ffi::OsString;
use std::rc::Rc;
use std::{env, path, process, ptr, time};

use clap::{Arg, ArgAction, Command as ClapCommand};
use futures::StreamExt as _;
use futures::channel::mpsc::unbounded;
use gpui::{Anchor, AnyWindowHandle, App, Application, Global, KeyBinding, WeakEntity, px};
use gpui_component::{Theme as ComponentTheme, init as init_components};
use gpui_windows::WindowsPlatform;
use nmt_agent_utils::{AgentEvent, AgentRoute, agent_process};
use nmt_config::local_state::{self, LocalState};
use nmt_config::{Config, enable_testing_mode, get, init};
use nmt_platform::set_job_management;
use nmt_platform::windows::ipc as platform_ipc;
use tracing::warn;
use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};

mod agent_pane;
mod cli;
mod error;
mod ipc;
mod logging;
mod pane_tree;
#[cfg(windows)]
mod remote;
mod tabs;
mod terminal;
mod ui;
mod utils;
mod window;
mod workspace;

use crate::agent_pane::AgentThreadDefaults;
use crate::cli::CliAction;
use crate::terminal::view::{
    CopyBlockCommand, CopyBlockOutput, NextBlock, PreviousBlock, RerunBlock, SendShiftTab, SendTab,
};
use crate::ui::{
    AppAssets, AppSettings, CloseTab, NewAgentTab, NewRemoteTab, NewTab, NewWindow, NewWorkspace,
    NextTab, NextWorkspace, PrevTab, PrevWorkspace, ResizePaneDown, ResizePaneLeft,
    ResizePaneRight, ResizePaneUp, ShowSettings, SplitDown, SplitLeft, SplitRight, SplitUp,
    ToggleSidebar,
};
use crate::window::{
    AppWindow, LastActiveWindow, ShellRegistry, WindowRegistry, selected_window_appearance,
};

struct StartupArgs {
    url: Option<String>,
    testing: bool,
}

struct StartupFiles {
    remembered_state: LocalState,
}

/// The concrete Windows platform, kept as a gpui global so settings toggles
/// can reach platform-level knobs (UI thread priority).
pub(crate) struct PlatformHandle(pub(crate) Rc<WindowsPlatform>);

impl Global for PlatformHandle {}

fn main() {
    let StartupArgs { url, testing } = parse_startup_args();
    run_app(url, testing);
}

fn parse_startup_args() -> StartupArgs {
    parse_startup_args_from(env::args_os())
}

fn parse_startup_args_from<I, T>(args: I) -> StartupArgs
where
    I: IntoIterator<Item = T>,
    T: Into<OsString>,
{
    let args = args.into_iter().map(Into::<OsString>::into);

    let matches = ClapCommand::new("NiumaTerm")
        .disable_help_flag(true)
        .arg(
            Arg::new("testing")
                .long("testing")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("new-tab")
                .long("new-tab")
                .value_name("PATH")
                .conflicts_with_all(["new-window", "url"]),
        )
        .arg(
            Arg::new("new-window")
                .long("new-window")
                .value_name("PATH")
                .conflicts_with_all(["new-tab", "url"]),
        )
        .arg(
            Arg::new("url")
                .index(1)
                .conflicts_with_all(["new-tab", "new-window"]),
        )
        .try_get_matches_from(args)
        .unwrap_or_else(|err| {
            eprintln!("{err}");
            process::exit(2);
        });

    StartupArgs {
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

fn run_app(argv_url: Option<String>, testing: bool) {
    agent_process().set_testing(testing);
    agent_process().set_hook_executable(
        utils::get_exe_dir()
            .join("NiumaTermHook.exe")
            .display()
            .to_string(),
    );

    if testing {
        enable_testing_mode();
    }

    // Hold the appender guard for the whole app lifetime; `main` blocks until exit.
    let _log_guard = logging::init_logging().expect("init logging");

    let startup_files = load_startup_files_or_exit();

    // Translations must be ready before any view exists so the first frame
    // already renders in the configured language.
    nmt_i18n::init(get().appearance.language.as_str());

    // A second launch forwards its action to the existing process so one process
    // URL (or an activate request) to the running instance and exits. A
    // malformed URL degrades to activate — the primary just comes forward.
    let argv_action = argv_url.map(|url| {
        cli::parse_nmt_url(&url).unwrap_or_else(|err| {
            warn!("ignoring command line: {err}");
            CliAction::Activate
        })
    });

    if !platform_ipc::try_become_primary(testing) {
        let action = argv_action.clone().unwrap_or(CliAction::Activate);
        match platform_ipc::send(&action.to_url(), time::Duration::from_secs(2), testing) {
            Ok(()) => return,
            Err(error) => warn!("primary instance pipe unreachable: {error}"),
        }
        // The mutex holder never answered (booting forever, or hung): serve
        // the user with a fresh primary rather than doing nothing.
    }

    let (cli_tx, mut cli_rx) = unbounded::<ipc::IpcAction>();

    ipc::spawn_pipe_server(cli_tx.clone(), testing);

    if let Some(action) = argv_action {
        // The primary's own argv URL joins the same dispatch path as
        // forwarded ones, applied after startup (and session restore).
        let _ = cli_tx.unbounded_send(ipc::IpcAction::Cli(action));
    }

    let platform = Rc::new(WindowsPlatform::new(false).expect("failed to initialize GPUI Windows"));

    platform.set_file_drop_description(nmt_i18n::i18n("app-drop-paste-path"));

    let platform_handle = platform.clone();

    Application::with_platform(platform)
        // Serve project icons + gpui-component's embedded icons so `svg().path()`
        // resolves both.
        .with_assets(AppAssets)
        .run(move |cx: &mut App| {
            // Initialize gpui-component (theme, root, component globals) before any
            // component renders. Themes without `[colors.ui]` retain the dark default.
            init_components(cx);

            // The component library localizes its own chrome (dialog buttons,
            // search placeholders) through a separate catalog; keep it on the
            // app language.
            gpui_component::set_locale(get().appearance.language.as_str());

            ui::apply_ui_theme(get().ui_theme.as_ref(), cx);

            let notification = &mut ComponentTheme::global_mut(cx).notification;
            notification.placement = Anchor::TopCenter;
            notification.margins.top = px(16.);

            cx.set_global(AppSettings::load());
            let agent_profiles = cx.global::<AppSettings>().agent_profiles.clone();
            agent_pane::updates::initialize(testing, &agent_profiles, cx);
            agent_pane::input_history::initialize(testing, cx);

            // Bring up the remote host service if it was left enabled. Runs on
            // its own runtime thread; failures only log.
            #[cfg(windows)]
            remote::reconcile(&nmt_config::get().remote_session);

            ui::apply_window_translucency(cx);

            set_job_management(cx.global::<AppSettings>().manage_subprocess_job);

            // Terminal and agent scrolling are their own elements carrying
            // their own switch; this one covers every container that scrolls
            // through a plain scroll handle, which is the rest of the app.
            let smooth_panels = cx.global::<AppSettings>().smooth_scrolling.panels_enabled();
            cx.set_smooth_wheel_scrolling(smooth_panels);

            // The platform remembers the choice and applies it to the vsync
            // thread when that spawns (after this closure returns).
            if cx.global::<AppSettings>().prioritize_ui_threads {
                platform_handle.set_ui_thread_priority(true);
            }

            cx.set_global(PlatformHandle(platform_handle));

            // Keep live behavior in sync on any settings change. Persistence is
            // deferred to when the settings dialog closes (see Shell::on_show_settings).
            cx.observe_global::<AppSettings>(|cx| {
                let agent_profiles = cx.global::<AppSettings>().agent_profiles.clone();
                agent_pane::updates::reconcile_profiles(&agent_profiles, cx);
                set_job_management(cx.global::<AppSettings>().manage_subprocess_job);

                let smooth_panels = cx.global::<AppSettings>().smooth_scrolling.panels_enabled();
                cx.set_smooth_wheel_scrolling(smooth_panels);

                // Opacity changes retint the theme and switch each window
                // between acrylic composition and opaque presentation.
                ui::apply_window_translucency(cx);

                // The component-library locale doubles as the change detector:
                // the observer fires on every settings edit (including theme
                // filter keystrokes), and only a real language switch should
                // pay for a full re-render of every window.
                let language = cx.global::<AppSettings>().language;
                let language_changed = &*gpui_component::locale() != language.as_str();
                if language_changed {
                    nmt_i18n::set_language(language.as_str());
                    gpui_component::set_locale(language.as_str());
                }

                let background = ui::window_background_appearance(cx);
                let appearance = selected_window_appearance(cx);

                let handles: Vec<_> = cx
                    .global::<ShellRegistry>()
                    .0
                    .iter()
                    .map(|entry| entry.handle)
                    .collect();

                for handle in handles {
                    handle
                        .update(cx, |_, window, cx| {
                            window.set_background_appearance(background);
                            window.set_appearance_override(Some(appearance), cx);
                            if language_changed {
                                window.refresh();
                            }
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
                KeyBinding::new("ctrl-shift-r", NewRemoteTab, Some("Shell")),
                KeyBinding::new("ctrl-shift-a", NewAgentTab, Some("Shell")),
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
                    agent_defaults: remembered_state.agent_defaults.clone(),
                };

                if let Err(err) = local_state::save(&clean) {
                    warn!("failed to clear sessions from local_state.toml: {err}");
                }
            }

            cx.set_global(WindowRegistry(Vec::new()));
            cx.set_global(ShellRegistry(Vec::new()));
            cx.set_global(LastActiveWindow(None));
            cx.set_global(AgentThreadDefaults::from_local_state(
                &remembered_state.agent_defaults,
            ));

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
                if let Err(error) = agent_pane::input_history::flush(cx) {
                    warn!("failed to flush Agent input history: {error}");
                }

                // Settings edits live in the global until something writes
                // them out. Closing the settings surface does that, and so
                // does quitting with it still open.
                cx.global::<AppSettings>().save();

                let save_session = cx.global::<AppSettings>().restore_last_session_when_opening;

                let state = LocalState {
                    windows: cx
                        .global::<WindowRegistry>()
                        .0
                        .iter()
                        .map(|(_, w)| w.to_local_state(save_session))
                        .collect(),
                    agent_defaults: cx.global::<AgentThreadDefaults>().to_local_state(),
                };

                if !state.windows.is_empty()
                    && let Err(err) = local_state::save(&state)
                {
                    warn!("failed to save local_state.toml: {err}");
                }

                async {}
            })
            .detach();

            for initial in initials {
                AppWindow::open(cx, initial);
            }
            agent_pane::updates::schedule_automatic_checks(cx);

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
    let config = Config::load_for_startup().unwrap_or_else(|err| {
        startup_error_and_exit("config.toml", &err.to_string());
    });
    init(config);

    let remembered_state = local_state::try_load().unwrap_or_else(|err| {
        startup_error_and_exit("local_state.toml", &err.to_string());
    });

    StartupFiles { remembered_state }
}

fn startup_error_and_exit(file: &str, error: &str) -> ! {
    show_startup_error_dialog(
        &nmt_i18n::i18n("startup-parse-error")
            .replace("{file}", file)
            .replace("{error}", error),
    );
    process::exit(1);
}

pub(crate) fn show_startup_error_dialog(message: &str) {
    let title: Vec<u16> = nmt_i18n::i18n("startup-configuration-error")
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let message: Vec<u16> = message.encode_utf16().chain(Some(0)).collect();
    unsafe {
        MessageBoxW(
            ptr::null_mut(),
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
    fn keeps_url_argument_for_normal_launch() {
        let StartupArgs { url, .. } =
            parse_startup_args_from(["NiumaTerm", "nmt://action/new_tab?path=C%3A%2FWorkspace"]);
        assert_eq!(
            url.as_deref(),
            Some("nmt://action/new_tab?path=C%3A%2FWorkspace")
        );
    }

    #[test]
    fn parses_shell_extension_path_flags() {
        let StartupArgs { url, .. } =
            parse_startup_args_from(["NiumaTerm", "--new-tab", r"C:\A Dir"]);
        assert_eq!(
            url.as_deref(),
            Some("nmt://action/new_tab?path=C%3A%5CA%20Dir")
        );

        let StartupArgs { url, .. } =
            parse_startup_args_from(["NiumaTerm", "--new-window", r"C:\A&B"]);
        assert_eq!(
            url.as_deref(),
            Some("nmt://action/new_window?path=C%3A%5CA%26B")
        );
    }

    #[test]
    fn parses_testing_mode() {
        let StartupArgs { url, testing } = parse_startup_args_from(["NiumaTerm", "--testing"]);
        assert!(testing);
        assert!(url.is_none());

        let StartupArgs { testing, .. } = parse_startup_args_from(["NiumaTerm"]);
        assert!(!testing);
    }
}

/// The most recently active window's shell, falling back to the newest open
/// window when none was activated yet (or the active one just closed).
fn last_active_shell(cx: &App) -> Option<(AnyWindowHandle, WeakEntity<ui::Shell>)> {
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
/// directory, then reuse an exact workspace, open it as a tab in the
/// best-matching workspace, or create a new window. Invalid targets only bring
/// the app forward.
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
                warn!("nmt:// target is not a directory: {}", path.display());
                foreground_last_active(cx);
                return;
            }
            path.clone()
        }
        CliAction::FocusNotification { .. } => unreachable!("handled above"),
    };
    match action {
        CliAction::NewTab { .. } => {
            // Prefer an exact-path workspace across all windows. The most
            // recently active window wins when duplicates already exist;
            // remaining windows are checked newest first.
            let last = cx.global::<LastActiveWindow>().0;
            let registry = cx.global::<ShellRegistry>();
            let mut targets = Vec::with_capacity(registry.0.len());

            if let Some(entry) = registry
                .0
                .iter()
                .find(|entry| Some(entry.window_id) == last)
            {
                targets.push((entry.handle, entry.shell.clone()));
            }

            targets.extend(
                registry
                    .0
                    .iter()
                    .rev()
                    .filter(|entry| Some(entry.window_id) != last)
                    .map(|entry| (entry.handle, entry.shell.clone())),
            );

            for (handle, shell) in targets {
                let activated = handle
                    .update(cx, |_, window, cx| {
                        shell
                            .update(cx, |shell, cx| {
                                if workspace::exact_match(&shell.workspaces.summaries(), &path)
                                    .is_none()
                                {
                                    return false;
                                }

                                shell.open_dir_tab(&path, window, cx);
                                true
                            })
                            .unwrap_or(false)
                    })
                    .unwrap_or(false);

                if activated {
                    return;
                }
            }

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

fn dispatch_focus_notification(route: &AgentRoute, notification_id: &str, cx: &mut App) {
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
    warn!("ignoring stale notification focus action");
}

fn dispatch_agent_event(event: AgentEvent, cx: &mut App) {
    if !cx.global::<AppSettings>().enable_agent_hooks {
        return;
    }

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

    warn!("ignoring agent event for unknown or closed route");
}

/// CLI `new_window`: a fresh window (default geometry) whose single
/// workspace is rooted at `path`.
fn open_window_at(path: &path::Path, cx: &mut App) {
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
