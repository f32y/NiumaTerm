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

mod channel;
mod client;
mod devices;
#[cfg(windows)]
mod host;
#[cfg(windows)]
mod keys;

pub use channel::*;
pub use client::*;
pub use devices::*;
#[cfg(windows)]
pub use host::*;
#[cfg(windows)]
pub use keys::*;

/// Mode prefix: Noise IK, client is already paired.
pub const CONNECT_MODE_IK: u8 = 0x01;
/// Mode prefix: Noise XX, client wants to redeem a pairing token.
pub const CONNECT_MODE_PAIR: u8 = 0x02;
