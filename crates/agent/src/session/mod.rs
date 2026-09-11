//! Provider selection, message delivery, and session lifecycle without GUI state.

use crate::update::ProviderKind;

mod backend;
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
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub mod test_support;
pub mod update_readiness;
pub mod workflows;

pub use crate::session::backend::{
    Backend, ConversationTitleRequest, RecoveryIdentity, RenameOutcome,
};
pub use crate::session::lifecycle::{RecoverySnapshot, RestorationReadiness, SessionRuntime};

#[cfg(test)]
mod tests;

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

/// Which agent backs this pane; the persisted tab snapshot stores the agent
/// name so future kinds can slot in without a schema change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentKind {
    Codex,
    Claude,
    DeepSeek,
}

impl AgentKind {
    /// Every kind a profile can select. Adding a kind here is what puts it in
    /// front of the user; the settings lists read this instead of repeating
    /// their own literals.
    ///
    /// The order is the order profiles are seeded in, and the first entry
    /// becomes a new installation's default profile, so a kind is appended
    /// rather than inserted.
    pub const ALL: [Self; 3] = [Self::Claude, Self::Codex, Self::DeepSeek];

    pub fn display(self) -> &'static str {
        match self {
            AgentKind::Codex => "Codex",
            AgentKind::Claude => "Claude",
            AgentKind::DeepSeek => "DeepSeek",
        }
    }

    /// `None` for unknown kinds (a newer snapshot), which degrade to a plain
    /// terminal tab instead of losing the tab.
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| {
            let candidate: &str = (*kind).into();
            candidate == id
        })
    }

    /// DeepSeek is installed outside the application update service.
    pub fn provider_kind(self) -> Option<ProviderKind> {
        match self {
            Self::Claude => Some(ProviderKind::Claude),
            Self::Codex => Some(ProviderKind::Codex),
            Self::DeepSeek => None,
        }
    }
}

#[cfg(test)]
mod ui_split_tests;

impl From<AgentKind> for &'static str {
    fn from(value: AgentKind) -> Self {
        match value {
            AgentKind::Codex => "codex",
            AgentKind::Claude => "claude",
            AgentKind::DeepSeek => "deepseek",
        }
    }
}
