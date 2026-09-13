//! Provider selection, message delivery, and session lifecycle without GUI state.

pub use nmt_profile::AgentKind;

pub use crate::session::backend::{
    Backend, ConversationTitleRequest, RecoveryIdentity, RenameOutcome,
};
pub use crate::session::lifecycle::{RecoverySnapshot, RestorationReadiness, SessionRuntime};

pub mod branch;
pub mod capabilities;
pub mod children;
pub mod commands;
pub mod controller;
pub mod delivery;
pub mod history;
pub mod input;
pub mod lifecycle;
pub mod naming;
pub mod restore;
pub mod settings;
pub mod team_capabilities;
pub mod team_recovery;

pub mod update_readiness;
pub mod workflows;

mod backend;

#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub mod test_support;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod ui_split_tests;

/// Encoded image data borrowed from a composed message.
#[derive(Clone, Copy)]
pub struct ImageAttachment<'a> {
    pub bytes: &'a [u8],
    pub media_type: &'a str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnsupportedOperation {
    Rename,
    Fork,
    FileRewind,
}

#[derive(Debug, PartialEq, Eq)]
pub enum OperationError {
    Unsupported(UnsupportedOperation),
    Failed(String),
}
