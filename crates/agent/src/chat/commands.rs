//! Slash commands and skills as the composer sees them.
//!
//! Where a command comes from decides who runs it, and how it takes arguments
//! decides what the palette offers after the name, so both travel with the
//! command rather than being inferred from it.

/// Which layer contributed a slash command. The UI uses this only for
/// deterministic precedence when two layers advertise the same name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlashCommandSource {
    Local,
    Adapter,
    Provider,
}

/// Shape of the input accepted after a command name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlashCommandArguments {
    None,
    Freeform,
    Choices,
    Skills,
}

/// When a command may run relative to a model turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlashCommandRunPolicy {
    Immediate,
    QueueUntilIdle,
    IdleOnly,
}

/// Backend-neutral command metadata used by the composer palette.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlashCommandInfo {
    /// Normalized protocol name without the leading slash.
    pub name: String,
    pub description: String,
    pub argument_hint: Option<String>,
    pub source: SlashCommandSource,
    pub arguments: SlashCommandArguments,
    pub run_policy: SlashCommandRunPolicy,
}

/// One provider-discovered skill. `path` is part of the identity because
/// Codex can publish the same skill name from multiple configuration scopes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
    pub path: String,
    pub scope: String,
    pub enabled: bool,
    pub display_name: Option<String>,
}

/// Complete skill-directory state for the current backend session. Errors
/// can coexist with usable entries when one configured working directory or
/// skill file fails to load.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SkillCatalog {
    pub skills: Vec<SkillInfo>,
    pub errors: Vec<String>,
}

/// Exact provider identity selected by the UI for a structured skill input.
/// The catalog is revalidated before this reference is sent to the backend.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillReference {
    pub name: String,
    pub path: String,
}

/// Immediate result of asking a backend to execute a slash command. Turn
/// lifecycle remains event-driven: `Accepted` does not imply `TurnStarted`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SlashCommandOutcome {
    Accepted,
    Completed { message: Option<String> },
    Rejected { message: String },
    NotReady,
}
