use crate::chat::{
    ContextComposition, ContextWindowUsage, Event, ForkCheckpoint, GoalStatus, Item, SessionStats,
    SessionSummary, SkillCatalog, SlashCommandInfo, SlashCommandOutcome, TeamDecisionRequest,
    TurnActivity,
};
use crate::session::branch::BranchUpdate;
use crate::session::controller::{SessionController, SessionFailure, SessionReady, SessionReplay};
use crate::session::input::QuestionCompletion;
use crate::session::lifecycle::Status;
use crate::transcript::TextField;

/// What the host must present after the conversation has applied a provider event.
/// Payloads move through this result once; no transcript snapshot is constructed.
pub enum SessionEffect {
    Unchanged,
    Changed,
    Ready(SessionReady),
    Commands(Vec<SlashCommandInfo>),
    Skills(SkillCatalog),

    CommandResult {
        name: String,
        outcome: SlashCommandOutcome,
        advance: bool,
    },

    TurnStarted {
        opened: bool,
    },

    ProviderTurnAccepted {
        id: String,
    },

    TeamDecision(TeamDecisionRequest),

    ProviderTurnFinished {
        id: String,
        error: Option<String>,
    },

    TurnCompleted {
        error: Option<String>,
        interrupted: bool,
    },

    OutputTokens(u64),
    ContextWindow(ContextWindowUsage),
    ContextComposition(ContextComposition),
    CompactionStarted,

    CompactionFinished {
        error: Option<String>,
    },

    Branch(BranchUpdate),
    ItemStarted(Item),
    ItemCompleted(Item),

    TextDelta {
        item_id: String,
        delta: String,
        field: TextField,
    },

    ApprovalRequested,
    ApprovalResolved,

    QuestionsRequested {
        index: usize,
    },

    QuestionsResolved,

    InputRequested {
        index: usize,
    },

    InputResolved(QuestionCompletion),
    BackgroundActivity,

    Workflows {
        activity_changed: bool,
    },

    History(Vec<SessionSummary>),
    SearchResults(Vec<SessionSummary>),
    ConfirmedPrompts(Vec<String>),
    Goal(Option<GoalStatus>),
    PlanMode(bool),
    Title(String),
    Stats(SessionStats),
    Replay(SessionReplay),
    StatusDetail(Option<TurnActivity>),
    ForkCheckpoints(Result<Vec<ForkCheckpoint>, String>),

    HostExited {
        message: String,
    },

    Error {
        message: String,
        fatal: bool,
        failure: SessionFailure,
    },

    EffortRejected {
        message: String,
    },
}

impl SessionController {
    /// Recheck the generation for every event, including events buffered from
    /// one provider message. An earlier event may have replaced the session.
    pub fn apply_event(&mut self, epoch: u64, event: Event) -> SessionEffect {
        if !self.runtime.is_current(epoch) {
            return SessionEffect::Unchanged;
        }

        let effect = match event {
            Event::Ready(settings) => self
                .prepare_ready(settings)
                .map_or(SessionEffect::Unchanged, SessionEffect::Ready),

            Event::Models(models) => {
                self.controls.models = models;

                SessionEffect::Changed
            }

            Event::ApprovalPresets { presets, current } => {
                self.controls.approval_presets = presets;
                self.controls.settings.approval = current;

                SessionEffect::Changed
            }

            Event::AgentPresets { presets, current } => {
                self.controls.agent_presets = presets;
                self.controls.agent_preset = current;

                SessionEffect::Changed
            }

            Event::EffortRejected { message, effort } => {
                self.controls.settings.effort = effort;

                SessionEffect::EffortRejected { message }
            }

            Event::Commands(commands) => {
                self.command_catalog = Some(commands.clone());

                SessionEffect::Commands(commands)
            }

            Event::Skills(catalog) => {
                self.skill_catalog = Some(catalog.clone());

                SessionEffect::Skills(catalog)
            }

            Event::SlashCommandResult { name, outcome } => {
                let advance = self.settle_command(&outcome);

                SessionEffect::CommandResult {
                    name,
                    outcome,
                    advance,
                }
            }

            Event::TurnStarted => SessionEffect::TurnStarted {
                opened: self.turn_started(),
            },

            Event::ProviderTurnAccepted { id } => SessionEffect::ProviderTurnAccepted { id },
            Event::TeamDecision(request) => SessionEffect::TeamDecision(request),

            Event::ProviderTurnFinished { id, error } => {
                SessionEffect::ProviderTurnFinished { id, error }
            }

            Event::TurnCompleted { error } => SessionEffect::TurnCompleted {
                error,
                interrupted: self.turn_completed(),
            },

            Event::TurnOutputTokensUpdated(tokens) => SessionEffect::OutputTokens(tokens),
            Event::ContextWindowUpdated(usage) => SessionEffect::ContextWindow(usage),

            Event::ContextCompositionUpdated(composition) => {
                SessionEffect::ContextComposition(composition)
            }

            Event::CompactionStarted => SessionEffect::CompactionStarted,
            Event::CompactionFinished { error } => SessionEffect::CompactionFinished { error },

            Event::FileRewindCompleted { error } => SessionEffect::Branch(
                self.branch
                    .files_completed(epoch, error.map_or(Ok(()), Err)),
            ),

            Event::ItemStarted(item) => SessionEffect::ItemStarted(item),
            Event::ItemCompleted(item) => SessionEffect::ItemCompleted(item),

            Event::AgentMessageDelta { item_id, delta } => SessionEffect::TextDelta {
                item_id,
                delta,
                field: TextField::Reply,
            },

            Event::ReasoningSummaryDelta { item_id, delta } => SessionEffect::TextDelta {
                item_id,
                delta,
                field: TextField::ReasoningSummary,
            },

            Event::CommandOutputDelta { item_id, delta } => SessionEffect::TextDelta {
                item_id,
                delta,
                field: TextField::CommandOutput,
            },

            Event::ApprovalRequested { description } => {
                self.input.ask_approval(description);

                SessionEffect::ApprovalRequested
            }

            Event::ApprovalResolved => {
                if self.input.resolve_approval(epoch) {
                    SessionEffect::ApprovalResolved
                } else {
                    SessionEffect::Unchanged
                }
            }

            Event::QuestionsRequested { questions } => SessionEffect::QuestionsRequested {
                index: self.input.receive_legacy(questions),
            },

            Event::QuestionsResolved => {
                if self.input.resolve_legacy(epoch) {
                    SessionEffect::QuestionsResolved
                } else {
                    SessionEffect::Unchanged
                }
            }

            Event::InputRequested(request) => match self.input.receive(&self.runtime, request) {
                Some(index) => SessionEffect::InputRequested { index },
                None => SessionEffect::Unchanged,
            },

            Event::InputResolved { id, resolution } => {
                let Some(mut completion) = self.input.resolve(epoch, &id, resolution) else {
                    return SessionEffect::Unchanged;
                };

                completion.started_turn = completion.message.is_some()
                    && completion.started_turn
                    && self.runtime.status() == Status::Idle;

                if completion.started_turn {
                    self.delivery.begin_turn();
                    self.runtime.turn_started();
                }

                SessionEffect::InputResolved(completion)
            }

            Event::InputSubmissionFailed { id, message } => {
                if self.input.submission_failed(epoch, &id, message) {
                    SessionEffect::Changed
                } else {
                    SessionEffect::Unchanged
                }
            }

            Event::BackgroundTasks(snapshot) => {
                if self.set_background_tasks(snapshot) {
                    SessionEffect::BackgroundActivity
                } else {
                    SessionEffect::Changed
                }
            }

            Event::BackgroundTaskTranscript { key, update } => {
                if self
                    .children
                    .transcripts
                    .entry(key)
                    .or_default()
                    .apply(update)
                {
                    SessionEffect::Changed
                } else {
                    SessionEffect::Unchanged
                }
            }

            Event::Workflows(snapshot) => SessionEffect::Workflows {
                activity_changed: self.workflows.set_snapshot(snapshot),
            },

            Event::WorkflowAgentTranscript {
                task_id,
                agent_id,
                items,
            } => {
                if self.workflows.apply_transcript(&task_id, &agent_id, items) {
                    SessionEffect::Changed
                } else {
                    SessionEffect::Unchanged
                }
            }

            Event::History(sessions) => SessionEffect::History(sessions),
            Event::SessionSearchResults(sessions) => SessionEffect::SearchResults(sessions),

            Event::QueuedPrompts(prompts) => {
                SessionEffect::ConfirmedPrompts(self.delivery.snapshot(prompts))
            }

            Event::GoalUpdated(goal) => {
                self.goal = goal;

                SessionEffect::Changed
            }

            Event::PlanModeUpdated(active) => {
                self.plan_mode = active;

                SessionEffect::Changed
            }

            Event::TitleUpdated(title) => {
                self.naming.named = true;

                SessionEffect::Title(title)
            }

            Event::SessionStatsUpdated(stats) => SessionEffect::Stats(stats),

            Event::Replay(turns) => self
                .prepare_replay(turns)
                .map_or(SessionEffect::Unchanged, SessionEffect::Replay),

            Event::StatusDetail(detail) => SessionEffect::StatusDetail(detail),

            Event::ForkCheckpoints(checkpoints) => {
                SessionEffect::Branch(self.branch.fork_checkpoints(&mut self.runtime, checkpoints))
            }

            Event::HostExited { message } => SessionEffect::HostExited { message },

            Event::Error { message, fatal } => {
                let failure = self.failed(&message, fatal);

                SessionEffect::Error {
                    message,
                    fatal,
                    failure,
                }
            }
        };

        self.record_content(effect)
    }
}
