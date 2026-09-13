use serde::{Deserialize, Serialize};

/// Which agent backs this pane; the persisted tab snapshot stores the agent
/// name so future kinds can slot in without a schema change.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    Codex,
    #[default]
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

    pub fn full_name(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Codex => "Codex",
            Self::DeepSeek => "DeepSeek Harness",
        }
    }

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
}

impl From<AgentKind> for &'static str {
    fn from(value: AgentKind) -> Self {
        match value {
            AgentKind::Codex => "codex",
            AgentKind::Claude => "claude",
            AgentKind::DeepSeek => "deepseek",
        }
    }
}
