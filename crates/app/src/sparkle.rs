//! The macOS updater.
//!
//! Sparkle replaces the whole application bundle from a signed archive and
//! relaunches, so none of the Restart Manager machinery the Windows updater is
//! built on has a counterpart here. What is shared is the setting: `[update]
//! check-updates` decides whether background checks run on either platform.
//!
//! The check interval and whether Sparkle may ask about automatic checks on
//! first launch are stamped into the packaged bundle, so they are not repeated
//! here. What is mirrored is what a user can change while the application runs:
//! whether to check at all, and which channel to follow.
//!
//! There is no relaunch hook. Sparkle sends the running application a quit event
//! before it installs, so the application terminates through AppKit and its own
//! quit handler writes out the window state and settings on the way; a hook
//! would only write the same files a second time.

use gpui::{App, Global};
use nmt_config::update::UpdateChannel;
use nmt_sparkle::{Channel, StartError, Updater};
use tracing::{info, warn};

use crate::ui::AppSettings;

/// Holds the updater for the lifetime of the process. Sparkle stops its
/// scheduled checks when the last reference goes away.
struct AppUpdate(Updater);

impl Global for AppUpdate {}

/// Start the updater and bring it in line with the current settings.
///
/// A build that cannot update says so once and is otherwise silent: with no
/// global installed, every entry point below turns into a no-op.
pub(crate) fn initialize(testing: bool, cx: &mut App) {
    // A test instance is started and stopped repeatedly and shares the packaged
    // bundle's identity; letting it check for updates would mean network
    // traffic, and eventually an update prompt, from a process nobody is
    // watching.
    if testing {
        return;
    }

    let settings = cx.global::<AppSettings>();
    let (enabled, channel) = (settings.update.check_updates, channel(settings));

    match Updater::start(channel) {
        Ok(updater) => {
            updater.set_automatic_checks(enabled);
            cx.set_global(AppUpdate(updater));
        }
        // A build assembled locally names no feed. That is the intended state
        // for it, not a failure.
        Err(StartError::NoFeedConfigured) => {
            info!("this build has no update feed; automatic updates are off");
        }
        Err(error) => warn!("the updater did not start: {error}"),
    }
}

/// Mirror the application's own settings onto Sparkle.
pub(crate) fn settings_changed(cx: &mut App) {
    let settings = cx.global::<AppSettings>();
    let (enabled, channel) = (settings.update.check_updates, channel(settings));

    if let Some(update) = cx.try_global::<AppUpdate>() {
        update.0.set_automatic_checks(enabled);
        update.0.set_channel(channel);
    }
}

/// The channel the user has chosen, in the terms Sparkle understands.
fn channel(settings: &AppSettings) -> Channel {
    match settings.update.channel {
        UpdateChannel::Stable => Channel::Stable,
        UpdateChannel::Nightly => Channel::Nightly,
    }
}

/// Check now on the user's behalf, showing Sparkle's own progress and result
/// windows. Runs whether or not background checking is on, which is the point
/// of having a menu item for it.
pub(crate) fn check_now(cx: &App) {
    if let Some(update) = cx.try_global::<AppUpdate>() {
        update.0.check_for_updates();
    }
}

/// Whether a check can be started right now. False while one is already
/// running, and for a build that has no updater at all.
pub(crate) fn can_check(cx: &App) -> bool {
    cx.try_global::<AppUpdate>()
        .is_some_and(|update| update.0.can_check_for_updates())
}
