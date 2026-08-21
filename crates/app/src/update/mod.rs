//! Checking whether the selected channel has published something newer than
//! the running build, on a schedule and on demand.

mod releases;

#[cfg(test)]
mod tests;

use std::time::Duration;

use gpui::{App, Global};
use nmt_version::Version;

use crate::ui::AppSettings;
pub(crate) use crate::update::releases::CheckError;
use crate::update::releases::{Release, supersedes};

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
    Failed(CheckError),
}

pub(crate) struct AppUpdate {
    status: Status,
    testing: bool,
}

impl Global for AppUpdate {}

pub(crate) fn initialize(testing: bool, cx: &mut App) {
    cx.set_global(AppUpdate {
        status: Status::Unknown,
        testing,
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
    if cx.global::<AppUpdate>().status == Status::Checking {
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
