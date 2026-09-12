#![cfg_attr(windows, windows_subsystem = "windows")]

pub(crate) use crate::i18n::{_rust_i18n_t, _rust_i18n_try_translate};

mod agent_updates;
mod agent_usage;
mod cli;
mod daily_usage;
mod i18n;
mod ipc;
mod keymap;
mod logging;
#[cfg(target_os = "macos")]
mod menu;
mod pane_tree;
mod profiling;
#[cfg(windows)]
mod remote;
#[cfg(target_os = "macos")]
mod sparkle;
mod tabs;
mod ui;
#[cfg(windows)]
mod update;
mod usage_refresh;
mod usage_sources;
mod window;
mod workspace;

#[cfg(test)]
mod tests;

use std::ffi::OsString;
use std::future::{Ready, ready};
use std::rc::Rc;
use std::{env, mem, path, process, time};

use app::agent_tab::{AgentThreadDefaults, input_history};
use app::assets::AppAssets;
use app::{syntax, utils};
use clap::{Arg, ArgAction, Command as ClapCommand};
use futures::StreamExt as _;
use futures::channel::mpsc::unbounded;
#[cfg(windows)]
use gpui::Global;
use gpui::{Anchor, AnyWindowHandle, App, Application, WeakEntity, WindowId, px};
use gpui_component::{Theme as ComponentTheme, init as init_components};
#[cfg(target_os = "macos")]
use gpui_macos::MacPlatform as Platform;
#[cfg(windows)]
use gpui_windows::WindowsPlatform as Platform;
use nmt_agent::{AgentEvent, AgentRoute, agent_process};
use nmt_config::local_state::{self, LocalState};
use nmt_config::{Config, enable_testing_mode, get, init};
use nmt_platform::ipc as platform_ipc;
use nmt_platform::window::show_error_dialog;
#[cfg(enable_profiling)]
use nmt_profiling::allocation::ProfilingAllocator;
use rust_i18n::t;
use tracing::warn;

use crate::cli::CliAction;
use crate::ui::AppSettings;
use crate::window::{
    AppWindow, LastActiveWindow, ShellRegistry, WindowRegistry, selected_window_appearance,
};

#[cfg(enable_profiling)]
#[global_allocator]
static ALLOCATOR: ProfilingAllocator = ProfilingAllocator;

struct StartupArgs {
    url: Option<String>,
    testing: bool,
    profiling: bool,

    /// The instance an update replaced, which this one must outlive before it
    /// may claim the single-instance mutex that instance still holds.
    await_exit: Option<u32>,
}

struct StartupFiles {
    remembered_state: LocalState,
}

/// The flag a freshly installed build is relaunched with, naming the process
/// it has to outlive. It lives here rather than with the updater because the
/// command line is parsed on every platform, whether one is built or not.
pub(crate) const AWAIT_EXIT_FLAG: &str = "--await-exit";

/// The concrete Windows platform, kept as a gpui global so settings toggles
/// can reach platform-level knobs (UI thread priority). The one knob behind it
/// is Windows-only, and so is the handle: elsewhere nothing would read it.
#[cfg(windows)]
pub(crate) struct PlatformHandle(pub(crate) Rc<Platform>);

#[cfg(windows)]
impl Global for PlatformHandle {}

fn main() {
    let StartupArgs {
        url,
        testing,
        profiling,
        await_exit,
    } = parse_startup_args_from(env::args_os());

    // Only a build that can replace itself has a predecessor to outlive.
    #[cfg(windows)]
    if let Some(pid) = await_exit {
        update::await_predecessor(pid);
    }

    #[cfg(not(windows))]
    let _ = await_exit;

    // Builds without performance collection accept these switches through
    // empty hooks and do not install the allocator wrapper.
    nmt_profiling::set_enabled(profiling);

    agent_process().set_testing(testing);

    agent_process().set_hook_executable(
        utils::get_exe_dir()
            .join("NmtAgentHook.exe")
            .display()
            .to_string(),
    );

    if testing {
        enable_testing_mode();
    }

    // Hold the appender guard for the whole app lifetime; `main` blocks until exit.
    let _log_guard = logging::init_logging(testing).expect("init logging");

    if profiling && !cfg!(enable_profiling) {
        warn!("--enable-profiling requires a build with --cfg enable_profiling");
    }

    let startup_files = load_startup_files_or_exit();

    // Translations must be ready before any view exists so the first frame
    // already renders in the configured language.
    rust_i18n::set_locale(get().appearance.language.into());

    // A second launch forwards its action to the existing process so one process
    // URL (or an activate request) to the running instance and exits. A
    // malformed URL degrades to activate — the primary just comes forward.
    let argv_action = url.map(|url| {
        cli::parse_nmt_url(&url).unwrap_or_else(|err| {
            warn!("ignoring command line: {err}");

            CliAction::Activate
        })
    });

    if !platform_ipc::try_become_primary(testing) {
        let action = argv_action.clone().unwrap_or(CliAction::Activate);
        let url: String = (&action).into();

        match platform_ipc::send(&url, time::Duration::from_secs(2), testing) {
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

    #[cfg(windows)]
    let platform = Rc::new(Platform::new(false).expect("failed to initialize GPUI Windows"));

    #[cfg(target_os = "macos")]
    let platform = Rc::new(Platform::new(false));

    // The drop hint and the UI-thread priority are Windows-backend controls:
    // one names the effect shown by the Explorer drag cursor, the other raises
    // the render and vsync threads against the Windows scheduler.
    #[cfg(windows)]
    platform.set_file_drop_description(t!("app-drop-paste-path"));

    #[cfg(windows)]
    let platform_handle = platform.clone();

    let app = Application::with_platform(platform)
        // Serve project icons + gpui-component's embedded icons so `svg().path()`
        // resolves both.
        .with_assets(AppAssets);

    // Installed on the builder because the handler lives on the platform, which
    // only the builder owns; the app context handed to `run` cannot reach it.
    app.on_reopen(reopen_after_last_window_closed);

    app.run(move |cx: &mut App| {
        profiling::initialize(cx);

        // Initialize gpui-component (theme, root, component globals) before any
        // component renders. Themes without `[colors.ui]` retain the dark default.
        init_components(cx);

        // An update is performed by the instance it replaces, so this
        // startup is where the files that instance renamed aside are
        // finally removable and where a package file it was too old to know
        // about gets installed. Syntax highlighting loads one of those
        // files, which is why this runs before it rather than beside the
        // rest of the update setup below.
        #[cfg(windows)]
        update::settle_previous_update();

        if let Err(error) = syntax::register_languages() {
            warn!("syntax highlighting is limited to built-in languages: {error}");
        }

        ui::apply_ui_theme(get().ui_theme.as_ref(), cx);

        let notification = &mut ComponentTheme::global_mut(cx).notification;

        notification.placement = Anchor::TopCenter;
        notification.margins.top = px(16.);

        cx.set_global(AppSettings::load());
        ui::install_terminal_settings(cx);
        ui::install_agent_settings(cx);

        let agent_profiles = cx.global::<AppSettings>().agent_profiles.clone();

        agent_updates::initialize(testing, &agent_profiles, cx);
        input_history::initialize(testing, cx);

        #[cfg(windows)]
        update::initialize(testing, cx);

        #[cfg(target_os = "macos")]
        sparkle::initialize(testing, cx);

        // Bring up the remote host service if it was left enabled. Runs on
        // its own runtime thread; failures only log.
        #[cfg(windows)]
        remote::reconcile(&nmt_config::get().remote_session);

        ui::apply_window_translucency(cx);

        // Terminal and agent scrolling are their own elements carrying
        // their own switch; this one covers every container that scrolls
        // through a plain scroll handle, which is the rest of the app.
        let enable_smooth_scrolling = cx
            .global::<AppSettings>()
            .appearance
            .smooth_scrolling
            .panels_enabled();

        cx.set_smooth_wheel_scrolling(enable_smooth_scrolling);

        // The platform remembers the choice and applies it to the vsync
        // thread when that spawns (after this closure returns).
        #[cfg(windows)]
        if cx.global::<AppSettings>().system.prioritize_ui_threads {
            platform_handle.set_ui_thread_priority(true);
        }

        #[cfg(windows)]
        cx.set_global(PlatformHandle(platform_handle));

        // Keep live behavior in sync on any settings change. Persistence is
        // deferred to when the settings dialog closes (see Shell::on_show_settings).
        cx.observe_global::<AppSettings>(on_settings_changed)
            .detach();

        keymap::bind(cx);

        // The bar shows each command's shortcut, so it is built once the
        // bindings above are registered.
        #[cfg(target_os = "macos")]
        menu::install(cx);

        // Restore local state; first run centers and starts one default tab.
        let remembered_state = startup_files.remembered_state.clone();

        let restore_last_session_when_opening = cx
            .global::<AppSettings>()
            .system
            .restore_last_session_when_opening;

        let mut initials: Vec<AppWindow> = if restore_last_session_when_opening {
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
        if !restore_last_session_when_opening
            && remembered_state.windows.iter().any(|w| w.session.is_some())
        {
            let windows: Vec<_> = initials.iter().map(|w| w.to_local_state(false)).collect();

            if let Err(err) = local_state::save_windows(&windows) {
                warn!("failed to clear sessions from local_state.toml: {err}");
            }
        }

        cx.set_global(WindowRegistry(Vec::new()));
        cx.set_global(ShellRegistry(Vec::new()));
        cx.set_global(LastActiveWindow(None));
        cx.set_global::<AgentThreadDefaults>((&remembered_state.agent_defaults).into());

        // A closed window is discarded — except the last one, whose
        // geometry and session the quit hook still has to write out. On
        // Windows that quit is immediate; on macOS the process stays alive,
        // and `reopen_after_last_window_closed` consumes the entry if the
        // user comes back through the Dock first.
        cx.on_window_closed(on_window_closed).detach();

        cx.on_app_quit(on_app_quit).detach();

        for initial in initials {
            AppWindow::open(cx, initial);
        }

        agent_updates::schedule_automatic_checks(cx);

        #[cfg(windows)]
        update::schedule_automatic_checks(cx);

        // Apply CLI actions (argv + forwarded over the IPC pipe) on the
        // foreground; windows above exist before the first poll.
        cx.spawn(async move |cx| {
            while let Some(action) = cli_rx.next().await {
                cx.update(|cx| match action {
                    ipc::IpcAction::Cli(action) => on_ipc_cli(action, cx),
                    ipc::IpcAction::Agent(event) => on_ipc_agent_hook(event, cx),
                });
            }
        })
        .detach();

        cx.activate(true);
    });
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
            Arg::new("enable-profiling")
                .long("enable-profiling")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("await-exit")
                .long(AWAIT_EXIT_FLAG.trim_start_matches('-'))
                .value_name("PID")
                .value_parser(clap::value_parser!(u32))
                .hide(true),
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
        profiling: matches.get_flag("enable-profiling"),
        await_exit: matches.get_one::<u32>("await-exit").copied(),
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

fn on_settings_changed(cx: &mut App) {
    let agent_profiles = cx.global::<AppSettings>().agent_profiles.clone();

    agent_updates::reconcile_profiles(&agent_profiles, cx);

    #[cfg(windows)]
    update::settings_changed(cx);

    #[cfg(target_os = "macos")]
    sparkle::settings_changed(cx);

    let smooth_panels = cx
        .global::<AppSettings>()
        .appearance
        .smooth_scrolling
        .panels_enabled();

    cx.set_smooth_wheel_scrolling(smooth_panels);

    // Opacity changes retint the theme and switch each window
    // between acrylic composition and opaque presentation.
    ui::apply_window_translucency(cx);

    // The shared locale doubles as the change detector:
    // the observer fires on every settings edit (including theme
    // filter keystrokes), and only a real language switch should
    // pay for a full re-render of every window.
    let language: &str = cx.global::<AppSettings>().appearance.language.into();
    let language_changed = &*rust_i18n::locale() != language;

    if language_changed {
        rust_i18n::set_locale(language);

        // AppKit holds the strings the bar was built from, so it
        // keeps the previous language until it is rebuilt.
        #[cfg(target_os = "macos")]
        menu::refresh(cx);
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
}

fn on_window_closed(cx: &mut App, window_id: WindowId) {
    if cx.any_window_keeps_app_alive() {
        cx.global_mut::<WindowRegistry>().remove(window_id);
    }

    cx.global_mut::<ShellRegistry>().remove(window_id);

    let last_active = cx.global_mut::<LastActiveWindow>();

    if last_active.0 == Some(window_id) {
        last_active.0 = None;
    }
}

fn on_app_quit(cx: &mut App) -> Ready<()> {
    if let Err(error) = input_history::flush(cx) {
        warn!("failed to flush Agent input history: {error}");
    }

    // Settings edits live in the global until something writes
    // them out. Closing the settings surface does that, and so
    // does quitting with it still open.
    if !cx.global::<AppSettings>().editing.discard_on_exit
        && let Err(error) = cx.global_mut::<AppSettings>().save()
    {
        warn!("failed to save settings on application shutdown: {error}");
    }

    let save_session = cx
        .global::<AppSettings>()
        .system
        .restore_last_session_when_opening;

    let windows: Vec<_> = cx
        .global::<WindowRegistry>()
        .0
        .iter()
        .map(|(_, w)| w.to_local_state(save_session))
        .collect();

    if !windows.is_empty()
        && let Err(err) = local_state::save_windows(&windows)
    {
        warn!("failed to save local_state.toml: {err}");
    }

    ready(())
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
    show_startup_error_dialog(&t!("startup-parse-error", file = file, error = error));

    process::exit(1);
}

pub(crate) fn show_startup_error_dialog(message: &str) {
    show_error_dialog(&t!("startup-configuration-error"), message);
}

/// The most recently active window's shell, falling back to the newest open
/// window when none was activated yet (or the active one just closed).
fn get_last_active_shell(cx: &App) -> Option<(AnyWindowHandle, WeakEntity<ui::Shell>)> {
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
    if let Some((handle, _)) = get_last_active_shell(cx) {
        let _ = handle.update(cx, |_, window, _| window.activate_window());
    }
}

/// Answer a click on the Dock icon that arrives with nothing on screen.
///
/// macOS keeps the process running once the last window closes, so the icon
/// stays in the Dock and AppKit routes the click here; with no handler the
/// click does nothing and quitting is the only way back into a running app.
/// AppKit also asks when every window is merely minimized or hidden, so an
/// existing window is brought forward rather than joined by a second one.
fn reopen_after_last_window_closed(cx: &mut App) {
    // The shell registry, not GPUI's window list: the menu layer builds one
    // popup window and reuses it for the life of the process, so GPUI always
    // has a window even when the last terminal window is gone. The registry
    // holds exactly the terminal windows and is pruned as each one closes.
    if cx.global::<ShellRegistry>().0.is_empty() {
        open_window_without_a_source(cx);
    } else {
        foreground_last_active(cx);
    }
}

/// Open a window for a command that has no window to open one from: the Dock
/// answer above, and `NewWindow` when no focused window is there to handle it.
///
/// Closing the last window leaves its registry entry behind so the quit hook
/// can still write out its geometry. With no shell left, that entry is the
/// stale one, and consuming it both reopens where the user left off and keeps
/// the new window from being recorded beside an entry whose window no longer
/// exists, which would otherwise restore two windows on the next launch. While
/// a window is still open — minimized, and so unfocused — every entry belongs
/// to a live window, and a default window is opened instead.
pub(crate) fn open_window_without_a_source(cx: &mut App) {
    let mut initial = AppWindow {
        bounds: None,
        session: None,
        sidebar_width: None,
        initial_cwd: None,
    };

    if cx.global::<ShellRegistry>().0.is_empty()
        && let Some((_, remembered)) = mem::take(&mut cx.global_mut::<WindowRegistry>().0)
            .into_iter()
            .next()
    {
        initial.bounds = remembered.bounds;
        initial.sidebar_width = remembered.sidebar_width;

        if cx
            .global::<AppSettings>()
            .system
            .restore_last_session_when_opening
        {
            initial.session = remembered.session;
        }
    }

    AppWindow::open(cx, initial);
}

/// Apply one `nmt://` action: validate the target
/// directory, then reuse an exact workspace, open it as a tab in the
/// best-matching workspace, or create a new window. Invalid targets only bring
/// the app forward.
fn on_ipc_cli(action: CliAction, cx: &mut App) {
    match action {
        CliAction::FocusNotification {
            route,
            notification_id,
        } => dispatch_focus_notification(&route, &notification_id, cx),

        CliAction::Activate => foreground_last_active(cx),

        CliAction::NewTab { path } => {
            let Some(path) = openable_directory(path, cx) else {
                return;
            };

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
            let Some((handle, shell)) = get_last_active_shell(cx) else {
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

        CliAction::NewWindow { path } => {
            let Some(path) = openable_directory(path, cx) else {
                return;
            };

            open_window_at(&path, cx);
        }
    }
}

/// The target of an open request, when there is something to open. A path that
/// is not a directory only brings the app forward: a tab or window over it
/// would have no working directory to run in.
fn openable_directory(path: path::PathBuf, cx: &mut App) -> Option<path::PathBuf> {
    if path.is_dir() {
        return Some(path);
    }

    warn!("nmt:// target is not a directory: {}", path.display());
    foreground_last_active(cx);

    None
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

fn on_ipc_agent_hook(event: AgentEvent, cx: &mut App) {
    if !cx.global::<AppSettings>().agent.enable_agent_hooks {
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
