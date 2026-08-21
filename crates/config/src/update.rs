//! Update settings persisted as the `[update]` section of `config.toml`.

use serde::{Deserialize, Serialize};
use toml_edit::{DocumentMut, value};

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

impl UpdateChannel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Nightly => "nightly",
        }
    }

    pub fn from_value(value: &str) -> Self {
        match value {
            "nightly" => Self::Nightly,
            _ => Self::Stable,
        }
    }
}

pub(crate) fn patch_document(doc: &mut DocumentMut, update: &UpdateConfig) {
    doc["update"]["check-updates"] = value(update.check_updates);
    doc["update"]["channel"] = value(update.channel.as_str());
}
