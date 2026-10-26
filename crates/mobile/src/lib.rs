//! The Rust core of the NiumaTerm mobile apps, exported through UniFFI.
//!
//! Every byte that crosses the network and every state machine the desktop
//! already tests stays in Rust: pairing, the encrypted channel, reconnects,
//! and the replicated agent view. The app receives flat, render-ready
//! records and sends typed commands, so it never parses a protocol message.

pub mod agent;
pub mod core;
pub mod error;
pub mod records;
pub mod terminal;

mod commands;
mod diagnostics;

uniffi::setup_scaffolding!();
