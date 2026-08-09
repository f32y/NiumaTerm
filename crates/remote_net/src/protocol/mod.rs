//! Transport protocol shared by the remote-session host, remote client, and
//! their tests: control/data frame codec, Noise handshake and transport
//! wrappers, and pairing-code encoding.
//!
//! This module is transport-agnostic: each encoded frame maps to exactly one
//! transport message (a WebSocket binary message carries one Noise ciphertext
//! which decrypts to one frame), so no outer length prefix is needed here.
//! Socket handling and device-authorization policy live in the surrounding
//! crate.

pub mod frames;
pub mod noise;
pub mod pairing;
pub mod types;

pub use frames::*;
pub use noise::*;
pub use pairing::*;
pub use types::*;
