//! DeepSeek Harness adapter.
//!
//! Unlike the Codex and Claude adapters, this one drives no CLI protocol over
//! stdio. The harness is a user-installed npm package whose `dsh web` host
//! serves a local HTTP and WebSocket interface, and that interface is the only
//! one a stock installation exposes: the ACP and SDK servers are separate
//! packages that refuse to start without a plugin composition the application
//! would then have to author and maintain.
//!
//! The host publishes an already-normalized session event stream and computes
//! its own render cards, so this adapter maps rather than reassembles. That is
//! why it is a fraction of the size of the two CLI adapters.

mod api;
mod commands;
mod events;
mod history;
mod host;
mod mapping;
mod models;
mod session;
mod usage;
mod version;

#[cfg(test)]
mod tests;

pub use crate::deepseek::host::{DEFAULT_EXECUTABLE, Host, HostError};
pub use crate::deepseek::session::Session;
pub use crate::deepseek::version::{SUPPORTED_VERSIONS, VersionSupport, describe_version};
