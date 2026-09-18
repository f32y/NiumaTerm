//! Schedules updater jobs and presents their outcomes in application windows.

use gpui::{AnyWindowHandle, App, Global, Window};
use nmt_platform::windows::window::show_error_dialog;
use nmt_updater::windows::{
    CHECK_INTERVAL, FIRST_CHECK_DELAY, FileUsePrompt, InstallAction, Status, Updater,
};
use rust_i18n::t;

use crate::ui::{AppSettings, WindowRegistry};
use crate::update::file_users;
use crate::utils::{get_exe_dir, on_runtime};

struct AppUpdate {
    updater: Updater,
    window: Option<AnyWindowHandle>,
}

impl Global for AppUpdate {}

pub(crate) fn initialize(testing: bool, cx: &mut App) {
    let settings = cx.global::<AppSettings>().config().update.clone();

    cx.set_global(AppUpdate {
        updater: Updater::new(
            env!("NIUMATERM_VERSION"),
            get_exe_dir(),
            &nmt_config::config_dir_path(),
            settings,
            testing,
        ),
        window: None,
    });
}

pub(crate) fn status(cx: &App) -> Status {
    cx.try_global::<AppUpdate>()
        .map_or(Status::Unknown, |update| update.updater.status().clone())
}

pub(crate) fn on_settings_changed(cx: &mut App) {
    let settings = cx.global::<AppSettings>().config().update.clone();

    if cx.global_mut::<AppUpdate>().updater.set_settings(settings) {
        check(cx);
    }
}

pub(crate) fn schedule_automatic_checks(cx: &mut App) {
    if cx.global::<AppUpdate>().updater.testing() {
        return;
    }

    cx.spawn(async move |cx| {
        cx.background_executor().timer(FIRST_CHECK_DELAY).await;

        loop {
            cx.update(|cx| {
                if cx.global::<AppUpdate>().updater.automatic_checks_enabled() {
                    check(cx);
                }
            });

            cx.background_executor().timer(CHECK_INTERVAL).await;
        }
    })
    .detach();
}

pub(crate) fn check(cx: &mut App) {
    let Some(check) = cx.global_mut::<AppUpdate>().updater.begin_check() else {
        return;
    };

    cx.refresh_windows();

    cx.spawn(async move |cx| {
        let checked = on_runtime(check.run()).await;

        cx.update(|cx| {
            if cx.global_mut::<AppUpdate>().updater.finish_check(checked) {
                cx.refresh_windows();
            }
        });
    })
    .detach();
}

pub(crate) fn install_now(window: &mut Window, cx: &mut App) {
    let update = cx.global_mut::<AppUpdate>();

    let Some(download) = update.updater.begin_install() else {
        return;
    };

    update.window = Some(window.window_handle());

    cx.refresh_windows();

    cx.spawn(async move |cx| {
        let downloaded = on_runtime(download.run()).await;

        cx.update(|cx| {
            let action = cx
                .global_mut::<AppUpdate>()
                .updater
                .finish_download(downloaded);

            handle_install_action(action, cx);
        });
    })
    .detach();
}

pub(crate) fn inspect_file_users(cx: &mut App) {
    let Some(users) = cx.global_mut::<AppUpdate>().updater.inspect_file_users() else {
        return;
    };

    cx.refresh_windows();

    cx.spawn(async move |cx| {
        let inspected = cx
            .background_executor()
            .spawn(async move { users.inspect() })
            .await;

        cx.update(|cx| {
            let action = cx
                .global_mut::<AppUpdate>()
                .updater
                .finish_inspection(inspected);

            handle_install_action(action, cx);
        });
    })
    .detach();
}

pub(crate) fn close_file_users(cx: &mut App) {
    let Some(users) = cx.global_mut::<AppUpdate>().updater.close_file_users() else {
        return;
    };

    cx.refresh_windows();

    cx.spawn(async move |cx| {
        let closed = cx
            .background_executor()
            .spawn(async move { users.close() })
            .await;

        cx.update(|cx| {
            let action = cx.global_mut::<AppUpdate>().updater.finish_closing(closed);

            handle_install_action(action, cx);
        });
    })
    .detach();
}

pub(crate) fn continue_install(cx: &mut App) {
    let action = cx.global_mut::<AppUpdate>().updater.continue_install();

    handle_install_action(action, cx);
}

pub(crate) fn cancel_install(cx: &mut App) {
    let update = cx.global_mut::<AppUpdate>();

    if update.updater.cancel_install() {
        update.window = None;

        cx.refresh_windows();
    }
}

pub(crate) fn complete_relaunch(cx: &mut App) {
    let update = cx.global_mut::<AppUpdate>();

    update.window = None;

    if update.updater.relaunch() {
        cx.quit();
    } else {
        cx.refresh_windows();
    }
}

fn handle_install_action(action: InstallAction, cx: &mut App) {
    match action {
        InstallAction::None => {
            cx.global_mut::<AppUpdate>().window = None;

            cx.refresh_windows();
        }
        InstallAction::InspectFileUsers => inspect_file_users(cx),
        InstallAction::Prompt(prompt) => show_file_use_prompt(prompt, cx),
        InstallAction::RecoveryWarning(applications) => show_recovery_warning(applications, cx),
        // Replacement may have renamed the running executable. Relaunch in
        // this callback before other work can rebuild state from that path.
        InstallAction::Relaunch => complete_relaunch(cx),
    }
}

fn pending_windows(cx: &App) -> Vec<AnyWindowHandle> {
    let mut handles: Vec<_> = cx.global::<AppUpdate>().window.into_iter().collect();

    if let Some(registry) = cx.try_global::<WindowRegistry>() {
        handles.extend(registry.windows().iter().map(|entry| entry.handle));
    }

    handles
}

fn show_file_use_prompt(prompt: FileUsePrompt, cx: &mut App) {
    for handle in pending_windows(cx) {
        if file_users::open_file_use_prompt(handle, prompt.clone(), cx) {
            cx.refresh_windows();

            return;
        }
    }

    // Without a live window there is nowhere to obtain the user's decision.
    cancel_install(cx);
}

fn show_recovery_warning(applications: Vec<String>, cx: &mut App) {
    for handle in pending_windows(cx) {
        if file_users::open_recovery_warning(handle, applications.clone(), cx) {
            cx.refresh_windows();

            return;
        }
    }

    let message = t!(
        "settings-about-recovery-warning-message",
        applications = &applications.join(", ")
    )
    .into_owned();

    show_error_dialog(&t!("settings-about-recovery-warning-title"), &message);

    complete_relaunch(cx);
}
