//! Checking whether the selected channel has published something newer than
//! the running build, on a schedule and on demand.

mod download;
mod file_users;
mod install;
mod releases;

#[cfg(test)]
mod tests;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui::{AnyWindowHandle, App, Global, Window};
use nmt_config::update::UpdateChannel;
use nmt_i18n::i18n;
use nmt_platform::windows::restart_manager::{
    AffectedApplication, FileUsage, RestartManagerError, RestartManagerSession,
};
use nmt_platform::windows::window::show_error_dialog;
use nmt_version::Version;
use tracing::warn;

use crate::ui::AppSettings;
pub(crate) use crate::update::releases::CheckError;
use crate::update::releases::{Release, supersedes};
use crate::utils::get_exe_dir;
use crate::window::ShellRegistry;

/// Hidden argument the instance started by an update is given, naming the
/// process it replaces. Hidden because nothing but that restart has a reason to
/// pass it.
pub(crate) const AWAIT_EXIT_FLAG: &str = "--await-exit";

/// Where a package is unpacked before any of it replaces an installed file.
const STAGING_DIRECTORY: &str = "update";

pub(crate) const APP_VERSION: &str = env!("NIUMATERM_VERSION");

/// Startup waits before the first check so opening windows are not competing
/// with a network request, and the recheck cadence keeps an installation that
/// stays open for days from drifting more than a few hours behind a release.
const FIRST_CHECK_DELAY: Duration = Duration::from_secs(5);
const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum Status {
    /// Nothing has been asked yet, which is what a build with checking turned
    /// off reports for as long as it stays off.
    #[default]
    Unknown,
    Checking,
    /// The channel has published nothing this build can be compared against,
    /// which is not the same as being current: an empty channel says nothing
    /// about what is running.
    NothingPublished,
    UpToDate,
    Available(Release),
    /// The package is being fetched and unpacked, or its captured plan is being
    /// applied without needing a user decision.
    Installing(Release),
    InspectingFileUse(Release),
    AwaitingFileUse(Release),
    ClosingFileUsers(Release),
    RecoveryWarning {
        release: Release,
        applications: Vec<String>,
    },
    Failed(CheckError),
    InstallFailed(InstallError),
}

impl Status {
    pub(crate) fn busy(&self) -> bool {
        matches!(self, Self::Checking) || self.installation_in_progress()
    }

    fn installation_in_progress(&self) -> bool {
        matches!(
            self,
            Self::Installing(_)
                | Self::InspectingFileUse(_)
                | Self::AwaitingFileUse(_)
                | Self::ClosingFileUsers(_)
                | Self::RecoveryWarning { .. }
        )
    }

    pub(crate) fn release(&self) -> Option<&Release> {
        match self {
            Self::Available(release)
            | Self::Installing(release)
            | Self::InspectingFileUse(release)
            | Self::AwaitingFileUse(release)
            | Self::ClosingFileUsers(release) => Some(release),
            Self::RecoveryWarning { release, .. } => Some(release),
            Self::Unknown
            | Self::Checking
            | Self::NothingPublished
            | Self::UpToDate
            | Self::Failed(_)
            | Self::InstallFailed(_) => None,
        }
    }
}

/// Why an update that was found could not be put in place. Each variant names
/// the step that refused, because what a user can do about it differs: a
/// release with nothing attached to it is not the same problem as an
/// installation directory this account may not write to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InstallError {
    /// The release has no package, or none published beside a checksum.
    NoPackage,
    Unreachable,
    /// What arrived is not what was published.
    Checksum,
    Unpack,
    NotWritable,
    Replace,
    /// The files were replaced, so the update did land; only the restart into
    /// it did not.
    Relaunch,
}

pub(crate) struct AppUpdate {
    status: Status,
    testing: bool,
    pending: Option<PendingInstall>,
    /// The settings the current status was produced under. Settings changes
    /// arrive as one undifferentiated notification, so the values are mirrored
    /// here to tell an update setting moving from a change to anything else.
    channel: UpdateChannel,
    checking_enabled: bool,
}

#[derive(Clone)]
struct PendingInstall {
    release: Release,
    staged: PathBuf,
    install: PathBuf,
    plan: install::InstallPlan,
    window: AnyWindowHandle,
    testing: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FileUsePromptReason {
    InUse,
    CheckFailed,
    RebootRequired,
    RemainingUsers,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FileUsePrompt {
    pub(crate) reason: FileUsePromptReason,
    pub(crate) applications: Vec<AffectedApplication>,
}

impl Global for AppUpdate {}

pub(crate) fn initialize(testing: bool, cx: &mut App) {
    let settings = cx.global::<AppSettings>();
    cx.set_global(AppUpdate {
        status: Status::Unknown,
        testing,
        pending: None,
        channel: settings.update_channel,
        checking_enabled: settings.check_updates,
    });
}

pub(crate) fn status(cx: &App) -> Status {
    cx.try_global::<AppUpdate>()
        .map_or(Status::Unknown, |update| update.status.clone())
}

/// The About page's Check button. It runs whether or not automatic checking is
/// on, which is the point of having it.
pub(crate) fn check_now(cx: &mut App) {
    check(cx);
}

/// The About page's Update button: fetch the release the last check found,
/// capture the files that differ from it, and restart into the result after any
/// required file-use decision.
///
/// Downloading and unpacking happen off the main thread; replacing does not.
/// The swap is a handful of renames within one directory, and it has to be the
/// last thing this process does to its installation before it quits, so running
/// it anywhere else would only add a window for something to happen in between.
pub(crate) fn install_now(window: &mut Window, cx: &mut App) {
    let Status::Available(release) = status(cx) else {
        return;
    };

    let testing = cx.global::<AppUpdate>().testing;
    let staging = nmt_config::config_dir_path().join(STAGING_DIRECTORY);
    let initiating_window = window.window_handle();
    let download_release = release.clone();

    set_status(Status::Installing(release.clone()), cx);

    cx.spawn(async move |cx| {
        let staged = cx
            .background_executor()
            .spawn(async move { download::stage(&download_release, &staging) })
            .await;

        cx.update(|cx| match staged {
            Ok(staged) => prepare_install(release, staged, initiating_window, testing, cx),
            Err(error) => fail_install(error, cx),
        });
    })
    .detach();
}

fn prepare_install(
    release: Release,
    staged: PathBuf,
    window: AnyWindowHandle,
    testing: bool,
    cx: &mut App,
) {
    let install = get_exe_dir();
    let plan = install::plan(&staged, &install);

    // A release the check offered should differ from what is installed in at
    // least its own label, so nothing to replace means the two disagree about
    // what is running. Restarting is still what was asked for.
    if plan.is_empty() {
        warn!("update: the published package matches what is installed");
    }

    let inspect_shell_extension = plan.contains(install::SHELL_EXTENSION_DLL);
    cx.global_mut::<AppUpdate>().pending = Some(PendingInstall {
        release,
        staged,
        install,
        plan,
        window,
        testing,
    });

    if inspect_shell_extension {
        inspect_file_users(cx);
    } else {
        continue_install(cx);
    }
}

fn inspect_file_users(cx: &mut App) {
    let Some(pending) = cx.global::<AppUpdate>().pending.clone() else {
        return;
    };
    let release = pending.release.clone();
    let dll = pending.install.join(install::SHELL_EXTENSION_DLL);
    set_status(Status::InspectingFileUse(release), cx);
    cx.refresh_windows();

    cx.spawn(async move |cx| {
        let result = cx
            .background_executor()
            .spawn(async move { file_usage(&dll) })
            .await;
        cx.update(|cx| finish_file_use_inspection(result, cx));
    })
    .detach();
}

fn file_usage(path: &Path) -> Result<FileUsage, RestartManagerError> {
    RestartManagerSession::for_files(&[path])?.file_usage()
}

fn finish_file_use_inspection(result: Result<FileUsage, RestartManagerError>, cx: &mut App) {
    let prompt = match classify_file_usage(result) {
        Ok(None) => {
            continue_install(cx);
            return;
        }
        Ok(Some(prompt)) => prompt,
        Err(error) => {
            warn!("update: checking shell-extension users failed: {error}");
            FileUsePrompt {
                reason: FileUsePromptReason::CheckFailed,
                applications: Vec::new(),
            }
        }
    };
    show_file_use_prompt(prompt, cx);
}

fn classify_file_usage(
    result: Result<FileUsage, RestartManagerError>,
) -> Result<Option<FileUsePrompt>, RestartManagerError> {
    let usage = result?;
    if usage.applications.is_empty() && usage.reboot_reasons.is_empty() {
        return Ok(None);
    }
    Ok(Some(FileUsePrompt {
        reason: if usage.reboot_reasons.is_empty() {
            FileUsePromptReason::InUse
        } else {
            FileUsePromptReason::RebootRequired
        },
        applications: usage.applications,
    }))
}

fn show_file_use_prompt(prompt: FileUsePrompt, cx: &mut App) {
    let Some(pending) = cx.global::<AppUpdate>().pending.clone() else {
        return;
    };
    set_status(Status::AwaitingFileUse(pending.release.clone()), cx);

    for handle in pending_windows(&pending, cx) {
        if file_users::open_file_use_prompt(handle, prompt.clone(), cx) {
            cx.refresh_windows();
            return;
        }
    }

    // Without a live window there is nowhere to obtain consent, so the staged
    // package remains untouched and the release can be attempted again later.
    cancel_install(cx);
}

fn pending_windows(pending: &PendingInstall, cx: &App) -> Vec<AnyWindowHandle> {
    let mut handles = vec![pending.window];
    if let Some(registry) = cx.try_global::<ShellRegistry>() {
        handles.extend(registry.0.iter().map(|entry| entry.handle));
    }
    handles
}

pub(crate) fn retry_file_use(cx: &mut App) {
    inspect_file_users(cx);
}

pub(crate) fn cancel_install(cx: &mut App) {
    let Some(pending) = cx.global_mut::<AppUpdate>().pending.take() else {
        return;
    };
    set_status(Status::Available(pending.release), cx);
    cx.refresh_windows();
}

pub(crate) fn continue_install(cx: &mut App) {
    let Some(release) = cx
        .global::<AppUpdate>()
        .pending
        .as_ref()
        .map(|pending| pending.release.clone())
    else {
        return;
    };
    set_status(Status::Installing(release), cx);

    match apply_pending_files(cx) {
        Ok(()) => complete_relaunch(cx),
        Err(error) => fail_install(error, cx),
    }
}

fn apply_pending_files(cx: &App) -> Result<(), InstallError> {
    let pending = cx
        .global::<AppUpdate>()
        .pending
        .as_ref()
        .expect("an install action needs its staged plan");
    install::apply(&pending.staged, &pending.install, &pending.plan)
}

pub(crate) fn complete_relaunch(cx: &mut App) {
    let Some(pending) = cx.global_mut::<AppUpdate>().pending.take() else {
        return;
    };

    // Nothing may consult the running executable's own path from here on. The
    // file it names has just been renamed aside, so anything rebuilt from it —
    // the context-menu registration, the notification identity — would name the
    // copy left behind instead of the installed one.
    match install::relaunch(&pending.install, pending.testing) {
        Ok(()) => cx.quit(),
        Err(error) => fail_install(error, cx),
    }
}

fn fail_install(error: InstallError, cx: &mut App) {
    cx.global_mut::<AppUpdate>().pending = None;
    set_status(Status::InstallFailed(error), cx);
    cx.refresh_windows();
}

trait FileUserSession {
    fn file_usage(&self) -> Result<FileUsage, RestartManagerError>;
    fn shutdown(&self) -> Result<(), RestartManagerError>;
    fn restart(&self) -> Result<(), RestartManagerError>;
}

impl FileUserSession for RestartManagerSession {
    fn file_usage(&self) -> Result<FileUsage, RestartManagerError> {
        RestartManagerSession::file_usage(self)
    }

    fn shutdown(&self) -> Result<(), RestartManagerError> {
        RestartManagerSession::shutdown(self)
    }

    fn restart(&self) -> Result<(), RestartManagerError> {
        RestartManagerSession::restart(self)
    }
}

trait FileUserSessionSource {
    type Session: FileUserSession;

    fn open(&self, path: &Path) -> Result<Self::Session, RestartManagerError>;
}

struct SystemSessionSource;

impl FileUserSessionSource for SystemSessionSource {
    type Session = RestartManagerSession;

    fn open(&self, path: &Path) -> Result<Self::Session, RestartManagerError> {
        RestartManagerSession::for_files(&[path])
    }
}

enum ClosePreparation<S> {
    Clear,
    Released {
        session: S,
        applications: Vec<AffectedApplication>,
    },
    Prompt(FileUsePrompt),
}

pub(crate) fn close_file_users(cx: &mut App) {
    let Some(pending) = cx.global::<AppUpdate>().pending.clone() else {
        return;
    };
    let dll = pending.install.join(install::SHELL_EXTENSION_DLL);
    set_status(Status::ClosingFileUsers(pending.release), cx);
    cx.refresh_windows();

    cx.spawn(async move |cx| {
        let prepared = cx
            .background_executor()
            .spawn(async move { prepare_close(&dll) })
            .await;

        match prepared {
            ClosePreparation::Clear => {
                cx.update(continue_install);
            }
            ClosePreparation::Prompt(prompt) => {
                cx.update(|cx| show_file_use_prompt(prompt, cx));
            }
            ClosePreparation::Released {
                session,
                applications,
            } => {
                cx.update(|cx| {
                    let applied = apply_pending_files(cx);
                    // Once the running executable has moved aside, no other UI
                    // callback may run before application recovery and relaunch:
                    // resolving current_exe during that gap would name the old
                    // copy instead of the installed executable.
                    let restarted = session.restart();
                    match applied {
                        Err(error) => fail_install(error, cx),
                        Ok(()) => finish_recovery(restarted, applications, cx),
                    }
                });
            }
        }
    })
    .detach();
}

fn prepare_close(path: &Path) -> ClosePreparation<RestartManagerSession> {
    prepare_close_with(&SystemSessionSource, path)
}

fn prepare_close_with<S>(source: &S, path: &Path) -> ClosePreparation<S::Session>
where
    S: FileUserSessionSource,
{
    let session = match source.open(path) {
        Ok(session) => session,
        Err(error) => {
            warn!("update: starting shell-extension shutdown failed: {error}");
            return check_failed_prompt();
        }
    };
    let usage = match session.file_usage() {
        Ok(usage) => usage,
        Err(error) => {
            warn!("update: refreshing shell-extension users failed: {error}");
            return check_failed_prompt();
        }
    };
    if !usage.reboot_reasons.is_empty() {
        return ClosePreparation::Prompt(FileUsePrompt {
            reason: FileUsePromptReason::RebootRequired,
            applications: usage.applications,
        });
    }
    if usage.applications.is_empty() {
        return ClosePreparation::Clear;
    }

    let applications = usage.applications;
    if let Err(error) = session.shutdown() {
        warn!("update: closing shell-extension users failed: {error}");
        if let Err(restart_error) = session.restart() {
            warn!("update: restoring partially closed applications failed: {restart_error}");
        }
        return match session.file_usage() {
            Ok(usage) => ClosePreparation::Prompt(FileUsePrompt {
                reason: if usage.reboot_reasons.is_empty() {
                    FileUsePromptReason::RemainingUsers
                } else {
                    FileUsePromptReason::RebootRequired
                },
                applications: usage.applications,
            }),
            Err(list_error) => {
                warn!("update: listing applications after failed shutdown failed: {list_error}");
                check_failed_prompt()
            }
        };
    }

    ClosePreparation::Released {
        session,
        applications,
    }
}

fn check_failed_prompt<S>() -> ClosePreparation<S> {
    ClosePreparation::Prompt(FileUsePrompt {
        reason: FileUsePromptReason::CheckFailed,
        applications: Vec::new(),
    })
}

fn finish_recovery(
    restarted: Result<(), RestartManagerError>,
    applications: Vec<AffectedApplication>,
    cx: &mut App,
) {
    if let Err(error) = &restarted {
        warn!("update: restarting shell-extension users failed: {error}");
    }
    let manual = recovery_application_names(&restarted, &applications);

    if manual.is_empty() {
        complete_relaunch(cx);
    } else {
        show_recovery_warning(manual, cx);
    }
}

fn recovery_application_names(
    restarted: &Result<(), RestartManagerError>,
    applications: &[AffectedApplication],
) -> Vec<String> {
    applications
        .iter()
        .filter(|application| restarted.is_err() || !application.restartable)
        .cloned()
        .map(application_name)
        .collect()
}

fn application_name(application: AffectedApplication) -> String {
    if application.name.is_empty() {
        format!("PID {}", application.process_id)
    } else {
        application.name
    }
}

fn show_recovery_warning(applications: Vec<String>, cx: &mut App) {
    let Some(pending) = cx.global::<AppUpdate>().pending.clone() else {
        return;
    };
    set_status(
        Status::RecoveryWarning {
            release: pending.release.clone(),
            applications: applications.clone(),
        },
        cx,
    );

    for handle in pending_windows(&pending, cx) {
        if file_users::open_recovery_warning(handle, applications.clone(), cx) {
            cx.refresh_windows();
            return;
        }
    }

    let message = i18n("settings-about-recovery-warning-message")
        .replace("{applications}", &applications.join(", "));
    show_error_dialog(i18n("settings-about-recovery-warning-title"), &message);
    complete_relaunch(cx);
}

/// Settle whatever an update that ran before this process started left behind.
pub(crate) fn settle_previous_update() {
    let install = get_exe_dir();
    let staging = nmt_config::config_dir_path().join(STAGING_DIRECTORY);

    install_staged_additions(&staging, &install);

    // Collect what a previous update renamed aside. The files are only
    // removable once whoever had them mapped has exited, which for the
    // executable this process replaced is now, and for the context-menu
    // extension is whenever Explorer next restarts.
    install::discard_previous(&install);

    // Whatever was unpacked before this process started has either been
    // installed already or belongs to an attempt that ended; either way a
    // retry fetches the package again rather than trusting what is lying here.
    let _ = fs::remove_dir_all(&staging);
}

/// Install package files the instance that performed the update did not know
/// to install, which is every file the release it installed added.
///
/// Each staged package sits in its own directory named after the release it was
/// fetched for, and only the one matching the installed executable contributes
/// anything, so an unrelated directory left here costs a version read.
fn install_staged_additions(staging: &Path, install: &Path) {
    let Ok(entries) = fs::read_dir(staging) else {
        return;
    };

    for entry in entries.flatten() {
        install::install_additions(&entry.path(), install);
    }
}

/// Wait for the instance an update replaced, so the single-instance check that
/// follows is not answered by a process on its way out.
pub(crate) fn await_predecessor(pid: u32) {
    if !nmt_platform::wait_for_exit(pid, install::PREDECESSOR_TIMEOUT) {
        warn!("update: the previous instance is still running; starting anyway");
    }
}

/// React to an update setting changing.
///
/// A status describes the channel it was fetched for, so keeping it across a
/// switch asserts something about the new channel that was never asked; it is
/// cleared instead. The check that replaces it still answers to the automatic
/// checking switch, which is the user saying not to reach the network unasked.
pub(crate) fn settings_changed(cx: &mut App) {
    let settings = cx.global::<AppSettings>();
    let channel = settings.update_channel;
    let checking_enabled = settings.check_updates;

    let update = cx.global_mut::<AppUpdate>();
    // An install is already committed to a release. Clearing its status would
    // report nothing in progress while the download or user decision remains,
    // so the new settings are recorded and answered once it has finished.
    if update.pending.is_some() || update.status.installation_in_progress() {
        update.channel = channel;
        update.checking_enabled = checking_enabled;

        return;
    }

    let switched = update.channel != channel;
    let turned_on = checking_enabled && !update.checking_enabled;
    update.channel = channel;
    update.checking_enabled = checking_enabled;
    if switched {
        update.status = Status::Unknown;
    }

    if update.testing || !checking_enabled || !(switched || turned_on) {
        return;
    }

    check(cx);
}

pub(crate) fn schedule_automatic_checks(cx: &mut App) {
    if cx.global::<AppUpdate>().testing {
        return;
    }
    cx.spawn(async move |cx| {
        cx.background_executor().timer(FIRST_CHECK_DELAY).await;
        loop {
            // Read the switch every tick rather than at startup: the user can
            // turn checking on and off while the app runs.
            cx.update(|cx| {
                if cx.global::<AppSettings>().check_updates {
                    check(cx);
                }
            });
            cx.background_executor().timer(CHECK_INTERVAL).await;
        }
    })
    .detach();
}

fn check(cx: &mut App) {
    // An install has already decided which release is being put in place, and a
    // check landing on top of it would replace that with an answer about a
    // release nothing is waiting for.
    if cx.global::<AppUpdate>().status.busy() || cx.global::<AppUpdate>().pending.is_some() {
        return;
    }
    let channel = cx.global::<AppSettings>().update_channel;
    set_status(Status::Checking, cx);

    cx.spawn(async move |cx| {
        let found = cx
            .background_executor()
            .spawn(async move { releases::latest(channel) })
            .await;
        cx.update(|cx| {
            // The channel can move while the request is out. A result for the
            // channel the user left says nothing about the one they chose, and
            // the switch has already started the check that does.
            if cx.global::<AppSettings>().update_channel != channel {
                return;
            }
            set_status(outcome(found), cx);
            cx.refresh_windows();
        });
    })
    .detach();
}

fn outcome(found: Result<Option<Release>, CheckError>) -> Status {
    let published = match found {
        Ok(Some(published)) => published,
        Ok(None) => return Status::NothingPublished,
        Err(error) => return Status::Failed(error),
    };

    // A build whose own label does not parse cannot be compared against
    // anything, so it is offered the release rather than told it is current.
    match Version::parse(APP_VERSION) {
        Some(current) if !supersedes(&current, &published) => Status::UpToDate,
        _ => Status::Available(published),
    }
}

fn set_status(status: Status, cx: &mut App) {
    cx.global_mut::<AppUpdate>().status = status;
}
