//! Remote sessions over the network: the host service, the client
//! connection, pairing records, and the network PTY a client tab reads.
//! Protocol state machines and codecs live in `nmt_remote_core`; this crate
//! adds sockets, the runtime, storage, and host terminals.

pub use crate::netwatch::notify_changed as notify_network_changed;
pub use crate::network_pty::NetworkPty;

pub mod client;
pub mod connection;
pub mod discovery;
#[cfg(feature = "host")]
pub mod host;
#[cfg(feature = "lan")]
pub mod lan;
#[cfg(feature = "host")]
pub mod local_view;
#[cfg(feature = "host")]
pub mod presence;
#[cfg(feature = "host")]
pub mod sessions;
pub mod store;

mod connection_stage;
mod direct;
#[cfg(all(feature = "lan", target_os = "macos"))]
mod discovery_macos;
#[cfg(all(feature = "lan", not(any(windows, target_os = "macos"))))]
mod discovery_mdns;
#[cfg(all(feature = "lan", windows))]
mod discovery_windows;
mod link;
mod netwatch;
mod network_pty;
#[cfg(feature = "host")]
mod push_sender;
mod relay;
#[cfg(feature = "host")]
mod relay_host;
mod secret;
#[cfg(feature = "host")]
mod stream;

#[cfg(test)]
mod client_tests;
#[cfg(test)]
mod direct_tests;
#[cfg(test)]
#[cfg(feature = "lan")]
mod lan_tests;
#[cfg(test)]
mod link_tests;
#[cfg(test)]
#[cfg(feature = "host")]
mod loopback_tests;
#[cfg(test)]
#[cfg(any(windows, target_vendor = "apple"))]
mod store_tests;
#[cfg(test)]
#[cfg(feature = "host")]
mod stream_tests;

use std::time::Duration;

/// Maximum time for a connection attempt or an individual handshake step.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
