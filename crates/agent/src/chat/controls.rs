//! The controls a conversation runs under, and the choices each offers.
//!
//! Every field is optional because a harness reports only the controls it has,
//! and an absent one means the harness decides rather than that the user chose
//! nothing.

/// Thread settings a chat UI lets the user pick. Field meanings are
/// per-backend: Codex sends them as overrides on every `turn/start`;
/// Claude stores its permission mode in `approval` and applies changes via
/// control requests before the next message (`approvals_reviewer`, `sandbox`,
/// and `tier` unused).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ThreadSettings {
    pub model: Option<String>,
    pub approval: Option<String>,
    pub approvals_reviewer: Option<String>,
    pub sandbox: Option<String>,
    pub effort: Option<String>,
    /// `None` is the normal tier: the model catalog only lists additional
    /// tiers, so normal is expressed as an explicit `serviceTier: null`
    /// (double-optional in the serialized payload — null resets, absent keeps).
    pub tier: Option<String>,
}

/// One selectable execution-permission preset a backend advertises.
///
/// A backend whose presets are fixed needs none of this, because the UI can
/// name them itself. This exists for one whose preset table is part of the
/// deployment, where a hard-coded list would offer values the deployment does
/// not serve and hide the ones it does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovalPreset {
    /// Submitted back verbatim when the user picks it.
    pub value: String,
    pub label: String,
    pub description: Option<String>,
}

/// One agent composition a conversation can be built from.
///
/// Separate from [`ApprovalPreset`] despite the matching shape: a permission
/// preset is a policy the session switches between at will, while this decides
/// which plugins compose the agent and can therefore only be chosen before the
/// conversation has run anything.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentPreset {
    /// Submitted back verbatim when the user picks it.
    pub value: String,
    pub label: String,
    pub description: Option<String>,
}

/// One entry of a backend's model catalog.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelInfo {
    pub model: String,
    pub display: String,
    /// `(tier id, tier name)` of the model's additional service tiers.
    pub tiers: Vec<(String, String)>,
    pub default_tier: Option<String>,
    /// Reasoning-effort levels the model supports; empty when the model has
    /// no effort control (or the backend keeps a global effort list instead).
    pub efforts: Vec<String>,
}
