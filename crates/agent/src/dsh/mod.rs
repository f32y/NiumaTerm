//! DeepSeek Harness adapter.
//!
//! Unlike the Codex and Claude adapters, this one drives no CLI protocol over
//! stdio. The harness is a user-installed npm package whose `dsh web` host
//! serves a local HTTP and WebSocket interface, and that interface is the only
//! one a stock installation exposes: the ACP and SDK servers are separate
//! packages that refuse to start without a plugin composition the application
//! would then have to author and maintain.
//!
//! The Remote API supplies raw session events, projection updates, and
//! answerable interactions over independent logical streams on one connection.

pub use crate::dsh::host::{DEFAULT_EXECUTABLE, Host, HostError};
pub use crate::dsh::session::Session;

pub(crate) use crate::dsh::host::{
    NPX_ARGUMENTS, NPX_EXECUTABLE, PNPM_DLX_ARGUMENTS, PNPM_DLX_EXECUTABLE,
};

mod api;
mod commands;
mod events;
mod frames;
mod history;
mod host;
mod mapping;
mod models;
mod presets;
mod projections;
mod session;
mod subagents;
mod workflows;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod version_tests;
