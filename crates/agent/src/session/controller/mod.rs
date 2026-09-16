//! Coordinates one provider conversation without editors, windows, or rendering.
//!
//! The host schedules blocking work and displays returned outcomes. Runtime,
//! delivery, recovery, and interactions advance together under one owner.

#[cfg(test)]
mod tests;

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;

use crate::background_task::{BackgroundTaskKey, BackgroundTaskSnapshot};
use crate::chat::{
    Event, GoalStatus, Item, ReplayTurn, SendOutcome, SessionSummary, SkillCatalog,
    SlashCommandInfo, SlashCommandOutcome, TeamDecisionRequest, ThreadSettings, TurnRetry,
};
use crate::progress::TaskList;
use crate::session::branch::{
    BranchCompletion, BranchFailure, BranchReplay, BranchUpdate, ConversationBranch,
};
use crate::session::capabilities::AgentCapabilities as _;
use crate::session::children::{ChildAgents, ChildTranscript};
use crate::session::commands::{CommandQueue, PendingSlashCommand};
use crate::session::delivery::{MessageDelivery, RecoverablePrompt};
use crate::session::input::{
    ApprovalOutcome, QuestionAction, QuestionCompletion, QuestionKey, SessionInput, Submission,
};
use crate::session::lifecycle::{InterruptOutcome, SessionRuntime, StartOutcome, Status};
use crate::session::naming::ConversationNaming;
use crate::session::restore::{ConversationRestore, ReadyAction, ReplayAction, SettingsSeed};
use crate::session::settings::ConversationSettings;
use crate::session::update_readiness::{ConversationWork, Readiness, prepare_stop};
use crate::session::workflows::WorkflowData;
use crate::session::{AgentKind, Backend, RecoveryIdentity};
use crate::transcript::TextField;
use crate::transcript::conversation::{ConversationImage, ConversationState, hidden};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubmissionBlock {
    QuestionResponse,
    ConversationChange,
    CommandStarting,
}

#[derive(Clone, Default)]
pub struct ReadyDefaults {
    pub stored: Option<ThreadSettings>,
    pub model: Option<String>,
    pub effort: Option<String>,
}

pub struct SessionController {
    kind: AgentKind,
    pub ready_defaults: ReadyDefaults,
    pub goal: Option<GoalStatus>,

    /// An explicit empty snapshot prevents older transcript tasks resurfacing.
    pub task_list: Option<TaskList>,

    pub plan_mode: bool,
    command_catalog: Option<Vec<SlashCommandInfo>>,
    skill_catalog: Option<SkillCatalog>,
    pub conversation: Rc<RefCell<ConversationState>>,
    pub pending_images: VecDeque<(String, Vec<Arc<ConversationImage>>)>,
    pub runtime: SessionRuntime,
    pub delivery: MessageDelivery,
    pub restore: ConversationRestore,
    pub naming: ConversationNaming,
    pub controls: ConversationSettings,
    pub input: SessionInput,
    pub branch: ConversationBranch,
    pub commands: CommandQueue,
    pub children: ChildAgents,
    pub workflows: WorkflowData,
}

impl SessionController {
    pub fn new(kind: AgentKind) -> Self {
        Self {
            kind,
            ready_defaults: ReadyDefaults::default(),
            goal: None,
            task_list: None,
            plan_mode: false,
            command_catalog: None,
            skill_catalog: None,
            conversation: Rc::new(RefCell::new(ConversationState::default())),
            pending_images: VecDeque::new(),
            runtime: SessionRuntime::default(),
            delivery: MessageDelivery::new(kind),
            restore: ConversationRestore::default(),
            naming: ConversationNaming::default(),
            controls: ConversationSettings {
                seed: SettingsSeed::Defaults,
                ..ConversationSettings::default()
            },
            input: SessionInput::default(),
            branch: ConversationBranch::default(),
            commands: CommandQueue::default(),
            children: ChildAgents::default(),
            workflows: WorkflowData::default(),
        }
    }

    /// Rejected sends leave accepted work and recovery data unchanged.
    pub fn submit(
        &mut self,
        text: String,
        send: impl FnOnce(&mut Backend, &str) -> SendOutcome,
        recovery: impl FnOnce() -> Option<RecoverablePrompt>,
    ) -> Result<SendOutcome, SubmissionBlock> {
        if self.input.has_submission() {
            return Err(SubmissionBlock::QuestionResponse);
        }

        if self.branch.holds_composer() {
            return Err(SubmissionBlock::ConversationChange);
        }

        if self.commands.awaiting_turn {
            return Err(SubmissionBlock::CommandStarting);
        }

        self.naming.sync(self.runtime.backend_mut());

        let outcome = self.runtime.send(|backend| send(backend, &text));

        let outcome = self.delivery.submit(outcome, text.clone(), recovery);

        if outcome == SendOutcome::StartedTurn {
            self.conversation.borrow_mut().start();

            self.push_item(Item::UserMessage { text: Some(text) });
        }

        Ok(outcome)
    }

    pub fn execute_command(&mut self, command: &PendingSlashCommand) -> SlashCommandOutcome {
        self.commands.execute(self.runtime.backend_mut(), command)
    }

    pub fn next_command(&mut self) -> Option<(String, SlashCommandOutcome)> {
        if self.runtime.status() != Status::Idle
            || self.commands.awaiting_turn
            || self.branch.holds_composer()
            || self.input.waiting()
        {
            return None;
        }

        let command = self.commands.queue.pop_front()?;
        let outcome = self.execute_command(&command);

        if matches!(
            outcome,
            SlashCommandOutcome::Rejected { .. } | SlashCommandOutcome::NotReady
        ) {
            self.commands.queue.clear();
        }

        Some((command.name, outcome))
    }

    fn settle_command(&mut self, outcome: &SlashCommandOutcome) -> bool {
        self.commands.settle(outcome, self.runtime.status())
    }

    /// Installation failure must retire accepted work from the failed start too.
    pub fn install(&mut self, epoch: u64, spawned: Result<Backend, String>) -> StartOutcome {
        let outcome = self.runtime.install(epoch, spawned);

        if matches!(outcome, StartOutcome::Failed(_)) {
            self.commands.clear();

            self.delivery.start_failed();
        }

        outcome
    }

    /// Begin a start and return its epoch, which later events must carry.
    pub fn starting(&mut self, recovery: Option<&RecoveryIdentity>) -> u64 {
        let epoch = self.runtime.begin_start();

        self.input.starting(epoch);

        self.restore.starting(epoch, recovery);

        if !self.branch.starting(epoch, recovery) {
            self.branch.clear();
        }

        self.naming.named = recovery.is_some();

        epoch
    }

    pub fn background_tasks(&self) -> Option<&BackgroundTaskSnapshot> {
        self.children
            .scoped(self.runtime.background_task_parent().as_ref())
    }

    pub fn background_task_transcript(&self, key: &BackgroundTaskKey) -> Option<&ChildTranscript> {
        self.children
            .transcript(self.runtime.background_task_parent().as_ref(), key)
    }

    pub fn background_activity(&self) -> (usize, usize) {
        self.children
            .activity(self.runtime.background_task_parent().as_ref())
    }

    pub fn update_readiness(&self, work: ConversationWork) -> Readiness {
        work.readiness(&self.runtime, &self.commands, &self.delivery)
    }

    pub fn prepare_update_stop(&mut self) {
        prepare_stop(&mut self.commands, &mut self.delivery);
    }

    pub fn push_item(&mut self, item: Item) {
        let images = if let Item::UserMessage { text: Some(text) } = &item {
            self.pending_images
                .iter()
                .position(|(pending, _)| pending == text)
                .and_then(|index| self.pending_images.remove(index))
                .map(|(_, images)| images)
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        self.conversation
            .borrow_mut()
            .push(self.delivery.turn(), item, images);
    }

    pub(crate) fn note_visible_output(&mut self) {
        self.delivery.visible_output();

        self.conversation.borrow_mut().visible_output();
    }

    pub fn publish_confirmed(&mut self) {
        while let Some(text) = self.delivery.pop_confirmed() {
            self.push_item(Item::UserMessage { text: Some(text) });
        }
    }

    pub fn start_item(&mut self, item: Item) {
        if let Item::UserMessage { text } = item {
            if let Some(text) = text.and_then(|text| self.delivery.echoed(&text)) {
                self.push_item(Item::UserMessage { text: Some(text) });
            }

            return;
        }

        if !hidden(&item) {
            self.note_visible_output();
        }

        if matches!(item, Item::AgentMessage { .. }) {
            self.delivery.agent_message();

            self.publish_confirmed();
        }

        self.push_item(item);
    }

    pub(crate) fn complete_item(&mut self, item: Item) {
        let Some(id) = item.id() else {
            return;
        };

        if !self.conversation.borrow().content.contains_item(id) {
            self.start_item(item);

            return;
        }

        if !hidden(&item) {
            self.note_visible_output();
        }

        self.conversation.borrow_mut().merge_completed(&item);
    }

    pub fn append_delta(&mut self, item_id: &str, delta: &str, field: TextField) {
        let visible = self
            .conversation
            .borrow_mut()
            .append_delta(item_id, delta, field)
            .is_some_and(|update| update.non_blank);

        if visible {
            self.note_visible_output();
        }
    }

    pub fn apply_replay(&mut self, replay: Vec<ReplayTurn>) {
        let answered_at = replay
            .iter()
            .flat_map(|turn| turn.items.iter())
            .filter_map(|entry| entry.at)
            .max();

        if let Some(at) = answered_at {
            let age = Duration::from_secs(
                u64::try_from(Utc::now().timestamp().saturating_sub(at)).unwrap_or(0),
            )
            .min(Duration::from_secs(3600));

            let now = Instant::now();

            self.conversation.borrow_mut().last_response_at =
                Some(now.checked_sub(age).unwrap_or(now));
        }

        for turn in replay {
            let id = self.delivery.replay_turn();

            self.conversation.borrow_mut().replay(id, turn);
        }
    }

    /// Compaction is shown on the turn it interrupts, so the flag lives on the
    /// live turn and the turn is marked changed either way.
    fn set_compacting(&mut self, compacting: bool) {
        let mut conversation = self.conversation.borrow_mut();

        conversation.live.set_compacting(compacting);

        conversation.changed_turn(self.delivery.turn());
    }

    /// Transcript side effects of the results that still leave the
    /// controller; the payload itself moves on to the host unchanged.
    fn record_content(&mut self, effect: SessionEffect) -> SessionEffect {
        match effect {
            SessionEffect::Error {
                message,
                fatal,
                failure,
            } => {
                self.note_visible_output();

                if fatal {
                    self.publish_confirmed();

                    self.conversation.borrow_mut().settle(self.delivery.turn());
                }

                self.push_item(Item::Error {
                    text: message.clone(),
                });

                SessionEffect::Error {
                    message,
                    fatal,
                    failure,
                }
            }
            SessionEffect::InputResolved(mut completion) => {
                if let Some(text) = completion.message.take() {
                    if completion.started_turn {
                        self.conversation.borrow_mut().start();
                    }

                    self.push_item(Item::UserMessage { text: Some(text) });
                }

                SessionEffect::InputResolved(completion)
            }
            SessionEffect::TurnCompleted { error, interrupted } => {
                if let Some(text) = &error {
                    let conversation = self.conversation.borrow();

                    let show = !conversation.turns.was_interrupted(self.delivery.turn())
                        && !conversation
                            .content
                            .turn_has_error(self.delivery.turn(), text);

                    drop(conversation);

                    if show {
                        self.push_item(Item::Error { text: text.clone() });
                    }
                }

                SessionEffect::TurnCompleted { error, interrupted }
            }
            effect @ (SessionEffect::ApprovalRequested | SessionEffect::InputRequested { .. }) => {
                self.note_visible_output();

                effect
            }
            effect => effect,
        }
    }

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
                self.controls.settings.agent_preset = current;

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
            Event::TurnOutputTokensUpdated(tokens) => {
                let mut conversation = self.conversation.borrow_mut();

                if conversation.live.set_output_tokens(tokens) {
                    conversation.changed_turn(self.delivery.turn());
                }

                SessionEffect::Changed
            }
            Event::ContextWindowUpdated(usage) => {
                self.conversation.borrow_mut().context_window_usage = Some(usage);

                SessionEffect::Changed
            }
            Event::ContextCompositionUpdated(composition) => {
                self.conversation.borrow_mut().context_composition = Some(composition);

                SessionEffect::Changed
            }
            Event::CompactionStarted => {
                self.note_visible_output();

                self.set_compacting(true);

                SessionEffect::Changed
            }
            Event::CompactionFinished { error } => {
                if let Some(text) = error {
                    self.push_item(Item::Error { text });
                }

                self.set_compacting(false);

                SessionEffect::Changed
            }
            Event::FileRewindCompleted { error } => SessionEffect::Branch(
                self.branch
                    .files_completed(epoch, error.map_or(Ok(()), Err)),
            ),
            Event::ItemStarted(item) => {
                self.start_item(item);

                SessionEffect::Changed
            }
            Event::ItemCompleted(item) => {
                self.complete_item(item);

                SessionEffect::Changed
            }
            Event::AgentMessageDelta { item_id, delta } => {
                self.append_delta(&item_id, &delta, TextField::Reply);

                SessionEffect::Changed
            }
            Event::ReasoningSummaryDelta { item_id, delta } => {
                self.append_delta(&item_id, &delta, TextField::ReasoningSummary);

                SessionEffect::Changed
            }
            Event::CommandOutputDelta { item_id, delta } => {
                self.append_delta(&item_id, &delta, TextField::CommandOutput);

                SessionEffect::Changed
            }
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
                let parent = self.runtime.background_task_parent();

                if self.children.set_snapshot(parent.as_ref(), snapshot) {
                    SessionEffect::BackgroundActivity
                } else {
                    SessionEffect::Changed
                }
            }
            Event::BackgroundTaskTranscript { key, update } => {
                if self.children.apply_transcript(key, update) {
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
                for text in self.delivery.snapshot(prompts) {
                    self.push_item(Item::UserMessage { text: Some(text) });
                }

                SessionEffect::Changed
            }
            Event::GoalUpdated(goal) => {
                self.goal = goal;

                SessionEffect::Changed
            }
            Event::TaskListUpdated(tasks) => {
                self.task_list = Some(tasks);

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
            Event::SessionStatsUpdated(stats) => {
                self.conversation.borrow_mut().session_stats = Some(stats);

                SessionEffect::Changed
            }
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

    /// Recovery removes the provisional turn. Capture the interrupted turn
    /// first so a late completion can still be attributed to the user's stop.
    pub fn interrupt_from_user(&mut self) -> UserInterruption {
        let turn = self.delivery.is_active().then_some(self.delivery.turn());
        let prompt = self.delivery.take_interrupted_prompt();
        let outcome = self.runtime.interrupt(turn);

        if let Some((turn, _)) = &prompt {
            let mut conversation = self.conversation.borrow_mut();

            conversation.live.discard();

            conversation.turns.forget(*turn);

            conversation.changed_turn(*turn);
        }

        UserInterruption { prompt, outcome }
    }

    pub fn respond_approval(&mut self, decision: &str) -> ApprovalOutcome {
        self.input.respond_approval(&mut self.runtime, decision)
    }

    pub fn submit_question(
        &mut self,
        key: QuestionKey,
        action: QuestionAction,
        now: Instant,
    ) -> Submission {
        if self.branch.holds_composer() || self.commands.awaiting_turn {
            return Submission::Ignored;
        }

        self.input
            .submit(&mut self.runtime, key, action, &self.controls.settings, now)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn restore_questions(&mut self) {
        self.input.restore(&mut self.runtime);
    }

    fn prepare_ready(&mut self, settings: ThreadSettings) -> Option<SessionReady> {
        let epoch = self.runtime.epoch();
        let branch = self.branch.ready(epoch);

        if branch.is_some() {
            self.clear_conversation();
        }

        let mut replay = match self.restore.ready(epoch) {
            ReadyAction::Ignore => return None,
            ReadyAction::Apply => None,
            ReadyAction::Replay(replay) => {
                self.clear_conversation();

                Some(replay)
            }
        };

        let branch = branch.map(|(completion, turns)| {
            self.apply_replay(turns);

            completion
        });

        let replaced = replay.is_some();

        if let Some(turns) = replay.take() {
            self.apply_replay(turns);
        }

        let reported_approval = settings.approval.clone();

        let selection = self.finish_ready(settings.clone());

        let approval = if self.kind.caps().approval_selection_is_a_command {
            self.runtime
                .backend_mut()
                .and_then(|backend| self.controls.apply_approval(backend, reported_approval))
        } else {
            None
        };

        Some(SessionReady {
            branch,
            replaced,
            selection,
            approval,
        })
    }

    fn prepare_replay(&mut self, turns: Vec<ReplayTurn>) -> Option<SessionReplay> {
        let epoch = self.runtime.epoch();

        let branch = match self.branch.replayed(epoch) {
            BranchReplay::Ignore => return None,
            BranchReplay::Unrelated => None,
            BranchReplay::Complete(completion) => {
                // A completed branch supersedes any pending restore before the
                // incoming replay is classified for the new conversation.
                self.clear_conversation();

                Some(completion)
            }
        };

        let replace = match self.restore.replayed(epoch) {
            ReplayAction::Ignore => return None,
            ReplayAction::Append => false,
            ReplayAction::Replace => {
                self.clear_conversation();

                true
            }
        };

        self.apply_replay(turns);

        Some(SessionReplay { branch, replace })
    }

    /// Apply host-supplied defaults after any restored content has been accepted.
    /// A model-selection refusal leaves the effective settings reported by the
    /// backend and returns its error for the host to present.
    fn finish_ready(&mut self, settings: ThreadSettings) -> Option<Result<(), String>> {
        self.input.restore(&mut self.runtime);

        let defaults = self.ready_defaults.clone();

        self.controls.ready(
            self.kind,
            settings,
            defaults.stored.as_ref(),
            defaults.model.as_deref(),
            defaults.effort.as_deref(),
        );

        let selection = if self.kind.caps().model_selection_is_a_request {
            self.runtime
                .backend_mut()
                .and_then(|backend| self.controls.apply_model(backend))
        } else {
            None
        };

        self.naming.sync(self.runtime.backend_mut());

        self.runtime.ready();

        selection
    }

    pub fn command_catalog(&self) -> Option<&[SlashCommandInfo]> {
        self.command_catalog.as_deref()
    }

    pub fn skill_catalog(&self) -> Option<&SkillCatalog> {
        self.skill_catalog.as_ref()
    }

    pub fn reset_for_restart(&mut self) -> Option<Backend> {
        let retiring = self.runtime.retire();

        self.clear_conversation();

        self.controls = ConversationSettings::default();
        self.command_catalog = None;
        self.skill_catalog = None;

        self.commands.clear();

        retiring
    }

    pub fn begin_branched_conversation(&mut self) {
        self.restore.cancel();

        self.controls.seed = SettingsSeed::None;
    }

    pub fn apply_model_selection(&mut self) -> Option<Result<(), String>> {
        self.controls.apply_model(self.runtime.backend_mut()?)
    }

    pub fn select_agent_preset(&mut self, preset: String) -> Option<Result<(), String>> {
        if self.controls.settings.agent_preset.as_deref() == Some(&preset) {
            return None;
        }

        let result = self.runtime.backend_mut()?.select_agent_preset(&preset);

        if result.is_ok() {
            self.controls.settings.agent_preset = Some(preset);
        }

        Some(result)
    }

    /// A provider or command may start work without a locally submitted prompt.
    /// Only that case reserves another turn; acknowledgements keep its number.
    fn turn_started(&mut self) -> bool {
        let opened = if self.commands.turn_started() {
            self.delivery.begin_turn();

            true
        } else {
            self.delivery.provider_started()
        };

        self.runtime.turn_started();

        if opened {
            self.conversation.borrow_mut().start();
        }

        self.publish_confirmed();

        opened
    }

    /// Completion consumes the matching interrupt and releases both work queues.
    fn turn_completed(&mut self) -> bool {
        let interrupted = self.runtime.turn_completed(self.delivery.turn());

        self.commands.turn_completed();

        self.delivery.completed();

        self.publish_confirmed();

        let turn = self.delivery.turn();

        let mut conversation = self.conversation.borrow_mut();

        if interrupted {
            conversation.turns.mark_interrupted(turn);
        }

        conversation.settle(turn);

        interrupted
    }

    pub fn failed(&mut self, message: &str, fatal: bool) -> SessionFailure {
        let branch = self.branch.failed(&mut self.runtime, message.to_owned());
        let resume_failed = self.restore.failed(&mut self.runtime);
        let cancelled_commands = fatal && !self.commands.queue.is_empty();

        if fatal {
            self.input.disconnect();

            self.runtime.exited(message);

            self.delivery.exited();

            self.commands.clear();
        } else if self.commands.awaiting_turn {
            self.commands.turn_completed();
        }

        SessionFailure {
            branch,
            resume_failed,
            cancelled_commands,
        }
    }

    pub fn disconnected(&mut self, message: &str) -> SessionFailure {
        let branch = self.branch.failed(
            &mut self.runtime,
            "session exited before branch readiness".into(),
        );

        let resume_failed = self.restore.failed(&mut self.runtime);

        self.runtime.exited(message);

        let cancelled_commands = self.commands.clear();

        self.input.disconnect();

        self.delivery.exited();

        SessionFailure {
            branch,
            resume_failed,
            cancelled_commands,
        }
    }

    /// Retain the backend while retiring conversation-specific state. Restarted
    /// turn numbers must not match an interrupt requested for the old content.
    pub fn clear_conversation(&mut self) {
        self.conversation.borrow_mut().clear();

        self.delivery.reset();

        self.pending_images.clear();

        self.runtime.clear_turn();

        self.branch.clear();

        self.restore.cancel();

        self.input.dismiss_approval();

        self.input.clear_questions();

        self.children.clear();

        self.goal = None;
        self.task_list = None;
        self.plan_mode = false;

        self.workflows.clear();
    }
}

pub struct SessionFailure {
    pub branch: Option<BranchFailure>,
    pub resume_failed: bool,
    pub cancelled_commands: bool,
}

pub struct UserInterruption {
    pub prompt: Option<(u64, RecoverablePrompt)>,
    pub outcome: InterruptOutcome,
}

pub struct SessionReady {
    pub branch: Option<BranchCompletion>,
    pub replaced: bool,
    pub selection: Option<Result<(), String>>,

    /// The outcome of sending a remembered permission preset to a harness
    /// that pinned its own default into the new conversation.
    pub approval: Option<Result<(), String>>,
}

pub struct SessionReplay {
    pub branch: Option<BranchCompletion>,
    pub replace: bool,
}

/// What the host must present after the conversation has applied a provider
/// event. Content updates are applied here and reported as `Changed`; only
/// results the host acts on beyond a repaint carry a payload.
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
    Branch(BranchUpdate),
    ApprovalRequested,
    ApprovalResolved,
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
    Title(String),
    Replay(SessionReplay),
    StatusDetail(Option<TurnRetry>),
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
