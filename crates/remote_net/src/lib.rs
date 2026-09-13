//! Networking engine for remote sessions, shared by both roles the app can
//! play: hosting local sessions for remote clients (`host`), and connecting
//! to a remote host as a client (`client`). The GUI layers UI on top; nothing
//! in here touches GPUI, so the whole engine is testable headless.
//!
//! Connection setup over the relay: the first WebSocket message from a client
//! carries a one-byte mode prefix ([`CONNECT_MODE_IK`] for authorized-device
//! connections, [`CONNECT_MODE_PAIR`] for first-contact pairing) followed by
//! the first Noise handshake message; the host picks its responder pattern
//! from that byte. Every later message is exactly one Noise ciphertext.

pub use crate::channel::*;
pub use crate::client::*;
pub use crate::devices::*;
#[cfg(windows)]
pub use crate::host::*;
#[cfg(windows)]
pub use crate::keys::*;
pub use crate::protocol::*;

#[cfg(windows)]
pub mod hub;
#[cfg(windows)]
pub mod net_pty;
pub mod protocol;

mod channel;
mod client;
mod devices;
#[cfg(windows)]
mod host;
#[cfg(windows)]
mod keys;
