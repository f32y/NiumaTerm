//! Process-global lifecycle for the remote-session host service, driven by the
//! Remote Session settings page.
//!
//! The host runs on its own tokio runtime thread (owned by `HostHandle`), so
//! this module is just a guarded slot plus reconciliation against the current
//! settings. Reconciliation is deliberately coarse: it runs on discrete events
//! (the enable toggle, dialog close, startup), never per keystroke, and
//! restarts the service only when the effective config actually changes.

use std::fs;
use std::path::PathBuf;

use nmt_config::remote_session::RemoteSessionConfig;
use nmt_i18n::i18n;
use nmt_platform::windows::environment::computer_name;
use nmt_remote_net::{
    AttachTarget, HostConfig, HostHandle, PairingCode, ProtocolSessionOptions, RemoteSession,
    hex_decode, hex_encode, load_or_create_keypair, open_remote_session,
};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::utils::get_data_dir;

struct RemoteHostState {
    handle: Option<HostHandle>,
    /// The (relay_url, access_token) the running handle was started with, so
    /// reconcile can detect a config change without restarting needlessly.
    started_with: Option<(String, String)>,
}

static STATE: Mutex<RemoteHostState> = Mutex::new(RemoteHostState {
    handle: None,
    started_with: None,
});

/// Start, stop, or restart the host service to match `config`. Safe to call
/// repeatedly; a no-op when nothing relevant changed.
pub fn reconcile(config: &RemoteSessionConfig) {
    let mut state = STATE.lock();

    let should_run =
        config.host_enabled && !config.relay_url.is_empty() && !config.access_token.is_empty();
    if !should_run {
        if let Some(handle) = state.handle.take() {
            handle.shutdown();
        }
        state.started_with = None;
        return;
    }

    let desired = (config.relay_url.clone(), config.access_token.clone());
    if state.handle.is_some() && state.started_with.as_ref() == Some(&desired) {
        return; // Already running with this config.
    }
    if let Some(handle) = state.handle.take() {
        handle.shutdown();
    }

    match HostHandle::start(HostConfig {
        relay_url: desired.0.clone(),
        access_token: desired.1.clone(),
        data_dir: get_data_dir(),
    }) {
        Ok(handle) => {
            state.handle = Some(handle);
            state.started_with = Some(desired);
        }
        Err(e) => {
            warn!("failed to start remote host service: {e}");
            state.started_with = None;
        }
    }
}

pub fn is_running() -> bool {
    STATE.lock().handle.is_some()
}

pub fn host_id() -> Option<String> {
    STATE.lock().handle.as_ref().map(|h| h.host_id().to_owned())
}

/// Issue a fresh one-time pairing code, or `None` if the host is not running.
pub fn begin_pairing() -> Option<PairingCode> {
    STATE.lock().handle.as_ref().map(|h| h.begin_pairing())
}

pub fn list_devices() -> Vec<nmt_remote_net::DeviceEntry> {
    STATE
        .lock()
        .handle
        .as_ref()
        .map(|h| h.list_devices())
        .unwrap_or_default()
}

pub fn revoke_device(public_key_hex: &str) {
    if let Some(handle) = STATE.lock().handle.as_ref() {
        if let Err(e) = handle.revoke_device(public_key_hex) {
            warn!("failed to revoke device: {e}");
        }
    }
}

// --- Client side: this machine connecting to a remote host ---------------

/// A host this machine has paired with, persisted under the data dir. The
/// public key is pinned at pairing time; later IK connections verify against
/// it, so a compromised relay cannot impersonate the host.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnownHost {
    pub name: String,
    pub relay_url: String,
    pub host_id: String,
    /// Hex-encoded X25519 public key.
    pub host_public_key: String,
}

fn device_key_path() -> PathBuf {
    get_data_dir().join("device-key.json")
}

fn known_hosts_path() -> PathBuf {
    get_data_dir().join("known_hosts.json")
}

pub fn known_hosts() -> Vec<KnownHost> {
    match fs::read(known_hosts_path()) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            // Losing the pinned host keys silently would look like the pairing
            // never happened, so make the file corruption visible.
            warn!("known_hosts.json is unreadable, treating as empty: {e}");
            Vec::new()
        }),
        Err(_) => Vec::new(),
    }
}

fn save_known_hosts(hosts: &[KnownHost]) {
    if let Err(e) = fs::write(
        known_hosts_path(),
        serde_json::to_vec_pretty(hosts).expect("serializable"),
    ) {
        warn!("failed to save known hosts: {e}");
    }
}

pub fn forget_host(host_id: &str) {
    let mut hosts = known_hosts();
    hosts.retain(|h| h.host_id != host_id);
    save_known_hosts(&hosts);
}

/// Pair with a host by redeeming a pairing code, persisting it as a known host
/// (blocking on the network round trip).
pub fn pair_with_code(code_text: &str, name: &str) -> Result<KnownHost, String> {
    let code = PairingCode::decode(code_text.trim()).map_err(|e| e.to_string())?;
    let device = load_or_create_keypair(&device_key_path()).map_err(|e| e.to_string())?;

    let host = KnownHost {
        name: name.to_owned(),
        relay_url: code.relay_url.clone(),
        host_id: code.host_id.clone(),
        host_public_key: hex_encode(&code.host_public_key),
    };

    nmt_remote_net::pair_device(code, device, hostname()).map_err(|e| e.to_string())?;

    let mut hosts = known_hosts();
    hosts.retain(|h| h.host_id != host.host_id);
    hosts.push(host.clone());
    save_known_hosts(&hosts);
    Ok(host)
}

/// Open (and attach to) a fresh session on a known host. Blocking.
pub fn connect_new_session(host: &KnownHost) -> Result<RemoteSession, String> {
    let device = load_or_create_keypair(&device_key_path()).map_err(|e| e.to_string())?;
    let host_public_key =
        hex_decode(&host.host_public_key).ok_or("stored host public key is not valid hex")?;
    open_remote_session(
        host.relay_url.clone(),
        host.host_id.clone(),
        host_public_key,
        device,
        AttachTarget::Open(ProtocolSessionOptions {
            shell: None,
            working_directory: None,
            cols: 80,
            rows: 24,
        }),
    )
    .map_err(|e| e.to_string())
}

fn hostname() -> String {
    computer_name().unwrap_or_else(|| i18n("remote-default-client-name").to_owned())
}
