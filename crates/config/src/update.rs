//! Update settings persisted as the `[update]` section of `config.toml`.

use serde::{Deserialize, Serialize};

use crate::defaults::default_bool_true;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UpdateConfig {
    /// Ask GitHub whether the selected channel has published something newer.
    /// The manual check on the About page stays available while this is off.
    #[serde(default = "default_bool_true", rename = "check-updates")]
    pub check_updates: bool,
    /// Which published channel counts as an update.
    #[serde(default, rename = "channel")]
    pub channel: UpdateChannel,
}

impl Default for UpdateConfig {
    fn default() -> Self {
        Self {
            check_updates: true,
            channel: UpdateChannel::default(),
        }
    }
}

/// The two ways a build is published. A missing or unreadable value lands on
/// stable, the channel a user who never chose is least surprised by.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum UpdateChannel {
    #[default]
    Stable,
    Nightly,
}

impl From<UpdateChannel> for &'static str {
    fn from(value: UpdateChannel) -> Self {
        match value {
            UpdateChannel::Stable => "stable",
            UpdateChannel::Nightly => "nightly",
        }
    }
}

impl From<&str> for UpdateChannel {
    fn from(value: &str) -> Self {
        match value {
            "nightly" => Self::Nightly,
            _ => Self::Stable,
        }
    }
}
