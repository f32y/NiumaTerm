//! Headless terminal sessions whose lifetime is independent of remote clients.
//!
//! Windows sessions are backed by ConPTY and keep running while all client
//! subscriptions are detached. Networking, authentication, and wire encoding
//! belong to the process that hosts this crate.

#[cfg(windows)]
mod windows;

#[cfg(windows)]
pub use windows::*;
