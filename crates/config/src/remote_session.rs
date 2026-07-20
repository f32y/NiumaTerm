//! Remote-session settings persisted as `[remote-session]` in `config.toml`.

use serde::{Deserialize, Serialize};
use toml_edit::{DocumentMut, value};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RemoteSession {
    /// Run newly opened terminals in the out-of-process SessionHub.
    #[serde(default)]
    pub enabled: bool,
}

pub(crate) fn patch_document(doc: &mut DocumentMut, remote_session: &RemoteSession) {
    doc["remote-session"]["enabled"] = value(remote_session.enabled);
}
