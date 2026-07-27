//! Remote-session settings persisted as the `[remote-session]` section of
//! `config.toml`. Only non-secret connection settings live here; the host's
//! private key and the authorized-device list are stored separately under the
//! per-user data directory (DPAPI-protected), not in the plaintext config.

use serde::{Deserialize, Serialize};
use toml_edit::{DocumentMut, value};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct RemoteSessionConfig {
    /// Whether this machine hosts its local sessions for remote clients.
    #[serde(default, rename = "host-enabled")]
    pub host_enabled: bool,
    /// Relay endpoint both host and clients dial, e.g.
    /// `wss://relay.example.com/ws`.
    #[serde(default, rename = "relay-url")]
    pub relay_url: String,
    /// Shared token the relay requires from hosts on registration. Clients
    /// never send it, so this gates host registration only.
    #[serde(default, rename = "access-token")]
    pub access_token: String,
}

pub(crate) fn patch_document(doc: &mut DocumentMut, remote: &RemoteSessionConfig) {
    doc["remote-session"]["host-enabled"] = value(remote.host_enabled);
    doc["remote-session"]["relay-url"] = value(&remote.relay_url);
    doc["remote-session"]["access-token"] = value(&remote.access_token);
}
