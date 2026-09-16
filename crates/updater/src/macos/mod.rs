//! Sparkle lifecycle and application update settings. The native framework
//! handles its own progress windows, signed bundle replacement, and relaunch.
//!
//! Sparkle sends the application a quit event before installing. The host's
//! existing quit handler can save settings and window state once, without a
//! separate updater callback repeating those writes.

pub use crate::macos::sparkle::StartError;

mod sparkle;

use nmt_config::update::{UpdateChannel, UpdateConfig};

use crate::macos::sparkle::{Channel, Updater as SparkleUpdater};

/// Keep this value alive on the main thread for as long as updates are enabled.
pub struct Updater(SparkleUpdater);

impl Updater {
    /// A test instance never starts Sparkle or reaches its configured feed.
    /// Unpackaged development builds report `NoFeedConfigured` to the host.
    pub fn start(settings: &UpdateConfig, testing: bool) -> Result<Option<Self>, StartError> {
        if testing {
            return Ok(None);
        }

        let updater = SparkleUpdater::start(channel(settings.channel))?;

        updater.set_automatic_checks(settings.check_updates);

        Ok(Some(Self(updater)))
    }

    pub fn set_settings(&self, settings: &UpdateConfig) {
        self.0.set_automatic_checks(settings.check_updates);

        self.0.set_channel(channel(settings.channel));
    }

    /// Manual checks remain available when automatic checking is disabled.
    pub fn check(&self) -> bool {
        if !self.can_check() {
            return false;
        }

        self.0.check_for_updates();

        true
    }

    pub fn can_check(&self) -> bool {
        self.0.can_check_for_updates()
    }
}

fn channel(channel: UpdateChannel) -> Channel {
    match channel {
        UpdateChannel::Stable => Channel::Stable,
        UpdateChannel::Nightly => Channel::Nightly,
    }
}
