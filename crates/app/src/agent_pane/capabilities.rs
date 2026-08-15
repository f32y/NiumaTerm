use crate::agent_pane::*;

/// What one harness can do. Behavior questions ask a named capability here
/// instead of comparing against a kind, so a call site reads as the question
/// it is actually asking and a new harness answers it once.
///
/// There is deliberately no `Default`: every field must be written out for a
/// new kind, which is what stops one from silently inheriting whichever
/// harness a scattered comparison happened to name.
pub(crate) struct Capabilities {
    /// Skills are named in the composer with a `$name` prefix and validated
    /// against a discovered catalog before the turn is sent.
    pub(crate) skill_references: bool,
    /// Files written since a chosen user message can be restored, which is
    /// what the `/rewind` command offers.
    pub(crate) file_rewind: bool,
    /// The harness reports workflow runs the pane can scope to its session.
    pub(crate) workflows: bool,
    /// Provider commands are discovered after startup rather than known at
    /// once, so an empty palette means "still loading" and says so instead of
    /// looking broken.
    pub(crate) async_command_discovery: bool,
    /// Another `Ready` arrives while the first turn initializes. A later one
    /// must keep the controls currently in use rather than restore the ones
    /// the CLI reports at startup.
    pub(crate) repeats_ready_during_init: bool,
    /// Recent conversations are read from the CLI's transcript directory
    /// instead of arriving over the protocol.
    pub(crate) filesystem_session_history: bool,
    /// Resuming replays the conversation's own thread controls, so remembered
    /// picks must not be seeded over them.
    pub(crate) resume_restores_thread_settings: bool,
    /// Resuming also replays the approval reviewer. A harness that restores
    /// the other controls but not this one leaves the pane to re-apply the
    /// remembered reviewer itself.
    pub(crate) resume_restores_approval_reviewer: bool,
    /// The launch fixes the model for the whole session because the system
    /// prompt is built from it, so a pick has to be resolved before spawning
    /// rather than sent as a later setting change.
    pub(crate) model_baked_into_launch: bool,
    /// Compaction is reported with enough detail for the transcript row to
    /// expand into what was summarized.
    pub(crate) expandable_compaction_rows: bool,
}

const CODEX: Capabilities = Capabilities {
    skill_references: true,
    file_rewind: false,
    workflows: false,
    async_command_discovery: false,
    repeats_ready_during_init: false,
    filesystem_session_history: false,
    resume_restores_thread_settings: true,
    resume_restores_approval_reviewer: false,
    model_baked_into_launch: false,
    expandable_compaction_rows: false,
};

const CLAUDE: Capabilities = Capabilities {
    skill_references: false,
    file_rewind: true,
    workflows: true,
    async_command_discovery: true,
    repeats_ready_during_init: true,
    filesystem_session_history: true,
    resume_restores_thread_settings: false,
    resume_restores_approval_reviewer: false,
    model_baked_into_launch: true,
    expandable_compaction_rows: true,
};

impl AgentKind {
    pub(crate) fn caps(self) -> &'static Capabilities {
        match self {
            AgentKind::Codex => &CODEX,
            AgentKind::Claude => &CLAUDE,
        }
    }
}
