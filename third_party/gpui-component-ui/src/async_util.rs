//! Cross-platform async utilities for both native and WASM targets.

// For native targets, re-export smol's primitives
// For WASM targets, use async-channel
#[cfg(target_family = "wasm")]
pub use async_channel::{Receiver, Sender, unbounded};
#[cfg(not(target_family = "wasm"))]
pub use smol::channel::{Receiver, Sender, unbounded};
