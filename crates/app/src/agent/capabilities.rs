use crate::agent::*;

/// What a backend does with a prompt submitted while a turn is running.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum QueuedPromptDelivery {
    /// The prompt joins the turn already in flight. The backend reports
    /// nothing about it, so assistant output arriving after the submit is the
    /// only sign it landed, and the turn's end is the last chance to say so.
    RunningTurn,
    /// The harness holds the prompt until the running turn ends and then opens
    /// a turn of its own for it. The prompt therefore heads that next turn,
    /// and drawing it into the finished one would put it above output written
    /// before it was ever submitted.
    FollowingTurn,
    /// The backend republishes its own pending inbox, so a prompt waiting
    /// behind the running turn is known rather than guessed at. Guessing
    /// beside it would show a message as sent while the snapshot still lists
    /// it as waiting.
    PendingInbox,
}

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
    /// A run's record lives on disk and is polled while the view is open,
    /// because the harness reports nothing about it once it has started. Where
    /// a run instead reports itself as it goes, polling would only re-read what
    /// the events already said, and a member's conversation is read once when
    /// the user opens it.
    pub(crate) workflows_read_from_disk: bool,
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
    /// A message can carry images beside its text. A harness without this
    /// refuses a pasted image rather than attaching one it cannot deliver.
    pub(crate) image_input: bool,
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
    /// An approval can be granted for the rest of the session, not just for the
    /// one call that asked. A harness whose answer vocabulary has no such
    /// outcome offers no button for it, because a button that silently degrades
    /// to allow-once would misreport what the user just agreed to.
    pub(crate) session_scoped_approval: bool,
    /// A skill is invoked by writing `/name` into an ordinary prompt, which the
    /// harness recognizes before the step runs. There is no invocation request,
    /// so a slash line naming a skill is a message rather than a command, and
    /// rejecting it as an unknown command would block the only way to use one.
    pub(crate) slash_skills_are_prompts: bool,
    /// Where a prompt submitted while a turn is running ends up, which is
    /// what decides when the transcript may draw it.
    pub(crate) queued_prompt_delivery: QueuedPromptDelivery,
    /// The conversation can be branched in front of a chosen prompt over the
    /// backend's own connection, which is what the `/fork` picker offers.
    /// Where the conversation is instead a file this side rewrites, `/fork`
    /// reaches the rewind picker that already cuts the same branch.
    pub(crate) session_fork: bool,
    /// A title can be pinned on the conversation, which is what `/rename`
    /// offers. Where titles are derived from the transcript and regenerated,
    /// there is nothing to pin them against.
    pub(crate) session_rename: bool,
    /// The harness indexes its own conversations and can be asked which of
    /// them mention a phrase, which is what `/find` offers.
    pub(crate) session_search: bool,
    /// The model pick is its own request the harness answers immediately, so a
    /// remembered pick seeded into the picker has to be pushed to reach the
    /// session at all. Where the pick instead rides the launch or the next
    /// turn, seeding the picker is the whole of applying it.
    pub(crate) model_selection_is_a_request: bool,
    /// Whether the harness can use every workspace directory or only the
    /// primary one. A harness that cannot take the whole set must say so in
    /// the tab rather than quietly working against one directory, and must
    /// never be widened to a common ancestor to look like one that can.
    pub(crate) multi_root_access: MultiRootAccess,
}

const CODEX: Capabilities = Capabilities {
    skill_references: true,
    file_rewind: false,
    workflows: false,
    workflows_read_from_disk: false,
    async_command_discovery: false,
    repeats_ready_during_init: false,
    filesystem_session_history: false,
    image_input: true,
    resume_restores_thread_settings: true,
    resume_restores_approval_reviewer: false,
    model_baked_into_launch: false,
    expandable_compaction_rows: false,
    session_scoped_approval: true,
    slash_skills_are_prompts: false,
    queued_prompt_delivery: QueuedPromptDelivery::RunningTurn,
    session_fork: true,
    session_rename: false,
    session_search: false,
    model_selection_is_a_request: false,
    multi_root_access: MultiRootAccess::Full,
};

const CLAUDE: Capabilities = Capabilities {
    skill_references: false,
    file_rewind: true,
    workflows: true,
    workflows_read_from_disk: true,
    async_command_discovery: true,
    repeats_ready_during_init: true,
    filesystem_session_history: true,
    image_input: true,
    resume_restores_thread_settings: false,
    resume_restores_approval_reviewer: false,
    model_baked_into_launch: true,
    expandable_compaction_rows: true,
    session_scoped_approval: true,
    slash_skills_are_prompts: false,
    queued_prompt_delivery: QueuedPromptDelivery::FollowingTurn,
    session_fork: false,
    session_rename: false,
    session_search: false,
    model_selection_is_a_request: false,
    multi_root_access: MultiRootAccess::Full,
};

/// `skill_references` is false because the harness has no structured skill
/// reference at all: a skill is named inside the prompt text, which
/// `slash_skills_are_prompts` is what carries. `file_rewind` is false for the
/// same kind of reason — DeepSeek Harness has no equivalent operation.
const DEEPSEEK: Capabilities = Capabilities {
    skill_references: false,
    file_rewind: false,
    workflows: true,
    workflows_read_from_disk: false,
    async_command_discovery: true,
    repeats_ready_during_init: false,
    filesystem_session_history: false,
    image_input: true,
    resume_restores_thread_settings: false,
    resume_restores_approval_reviewer: false,
    model_baked_into_launch: false,
    expandable_compaction_rows: true,
    session_scoped_approval: false,
    slash_skills_are_prompts: true,
    queued_prompt_delivery: QueuedPromptDelivery::PendingInbox,
    session_fork: true,
    session_rename: true,
    session_search: true,
    model_selection_is_a_request: true,
    // The installed Harness resolves one workspace root per session and its
    // workspace-write policy has no additional writable roots, so a
    // multi-directory workspace reduces to its primary directory and the tab
    // says which directories that leaves out. When a supported version
    // publishes a per-session multi-root policy this becomes `Full` and the
    // adapter passes every selected root.
    multi_root_access: MultiRootAccess::PrimaryOnly,
};

impl AgentKind {
    pub(crate) fn caps(self) -> &'static Capabilities {
        match self {
            AgentKind::Codex => &CODEX,
            AgentKind::Claude => &CLAUDE,
            AgentKind::DeepSeek => &DEEPSEEK,
        }
    }
}
