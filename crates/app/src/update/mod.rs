//! Checking whether the selected channel has published something newer than
//! the running build, on a schedule and on demand.

mod download;
mod install;
mod releases;

#[cfg(test)]
mod tests;

use std::fs;
use std::path::Path;
use std::time::Duration;

use gpui::{App, Global};
use nmt_config::update::UpdateChannel;
use nmt_version::Version;
use tracing::warn;

use crate::ui::AppSettings;
pub(crate) use crate::update::releases::CheckError;
use crate::update::releases::{Release, supersedes};
use crate::utils::get_exe_dir;

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
    /// The package is being fetched and unpacked. It ends either in a restart
    /// into the new build or in `InstallFailed`, so nothing observes success.
    Installing(Release),
    Failed(CheckError),
    InstallFailed(InstallError),
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
    /// The settings the current status was produced under. Settings changes
    /// arrive as one undifferentiated notification, so the values are mirrored
    /// here to tell an update setting moving from a change to anything else.
    channel: UpdateChannel,
    checking_enabled: bool,
}

impl Global for AppUpdate {}

pub(crate) fn initialize(testing: bool, cx: &mut App) {
    let settings = cx.global::<AppSettings>();
    cx.set_global(AppUpdate {
        status: Status::Unknown,
        testing,
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
/// replace the files that differ from it, and restart into the result.
///
/// Downloading and unpacking happen off the main thread; replacing does not.
/// The swap is a handful of renames within one directory, and it has to be the
/// last thing this process does to its installation before it quits, so running
/// it anywhere else would only add a window for something to happen in between.
pub(crate) fn install_now(cx: &mut App) {
    let Status::Available(release) = status(cx) else {
        return;
    };

    let testing = cx.global::<AppUpdate>().testing;
    let staging = nmt_config::config_dir_path().join(STAGING_DIRECTORY);

    set_status(Status::Installing(release.clone()), cx);

    cx.spawn(async move |cx| {
        let staged = cx
            .background_executor()
            .spawn(async move { download::stage(&release, &staging) })
            .await;

        let _ = cx.update(
            |cx| match staged.and_then(|staged| finish(&staged, testing)) {
                Ok(()) => cx.quit(),
                Err(error) => {
                    set_status(Status::InstallFailed(error), cx);
                    cx.refresh_windows();
                }
            },
        );
    })
    .detach();
}

fn finish(staged: &Path, testing: bool) -> Result<(), InstallError> {
    let install = get_exe_dir();

    // A release the check offered should differ from what is installed in at
    // least its own label, so nothing to replace means the two disagree about
    // what is running. Restarting is still what was asked for.
    if install::apply(staged, &install)?.is_empty() {
        warn!("update: the published package matches what is installed");
    }

    // Nothing may consult the running executable's own path from here on. The
    // file it names has just been renamed aside, so anything rebuilt from it —
    // the context-menu registration, the notification identity — would name the
    // copy left behind instead of the installed one.
    install::relaunch(&install, testing)
}

/// Collect what a previous update renamed aside. The files are only removable
/// once whoever had them mapped has exited, which for the executable this
/// process replaced is now, and for the context-menu extension is whenever
/// Explorer next restarts.
pub(crate) fn discard_replaced_files() {
    install::discard_previous(&get_exe_dir());

    // Whatever was unpacked before this process started has either been
    // installed already or belongs to an attempt that ended; either way a
    // retry fetches the package again rather than trusting what is lying here.
    let _ = fs::remove_dir_all(nmt_config::config_dir_path().join(STAGING_DIRECTORY));
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
    // report nothing in progress while the download continues, so the new
    // settings are recorded and answered once it has finished.
    if matches!(update.status, Status::Installing(_)) {
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
            let _ = cx.update(|cx| {
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
    if matches!(
        cx.global::<AppUpdate>().status,
        Status::Checking | Status::Installing(_)
    ) {
        return;
    }
    let channel = cx.global::<AppSettings>().update_channel;
    set_status(Status::Checking, cx);

    cx.spawn(async move |cx| {
        let found = cx
            .background_executor()
            .spawn(async move { releases::latest(channel) })
            .await;
        let _ = cx.update(|cx| {
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
        Some(current) => match Version::parse(&published.label) {
            Some(candidate) if !supersedes(&current, &candidate) => Status::UpToDate,
            _ => Status::Available(published),
        },
        None => Status::Available(published),
    }
}

fn set_status(status: Status, cx: &mut App) {
    cx.global_mut::<AppUpdate>().status = status;
}
