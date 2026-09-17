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
use serde_json::Value;

use crate::background_task::{BackgroundTaskKey, BackgroundTaskSnapshot};
use crate::chat::{
    Event, ForkCheckpoint, GoalStatus, Item, QueuedPrompt, ReplayTurn, SendOutcome, SessionScope,
    SessionSummary, SkillCatalog, SlashCommandInfo, SlashCommandOutcome, SlashCommandRunPolicy,
    TeamDecisionRequest, ThreadSettings, TurnRetry,
};
use crate::claude_code::sessions::{ClaudeCheckpoint, ClaudeFork};
use crate::progress::TaskList;
use crate::session::branch::{
    BranchCompletion, BranchError, BranchFailure, BranchReplay, BranchUpdate, CheckpointRead,
    ConversationBranch, ForkRequest, PromptTarget, RewindAction,
};
use crate::session::children::{ChildAgents, ChildTranscript};
use crate::session::commands::{CommandAdmission, CommandQueue, PendingSlashCommand};
use crate::session::delivery::{MessageDelivery, RecoverablePrompt};
use crate::session::input::{
    ApprovalOutcome, QuestionAction, QuestionCompletion, QuestionKey, SessionInput, Submission,
};
use crate::session::lifecycle::{
    InterruptOutcome, RecoverySnapshot, SessionRuntime, StartOutcome, Status,
};
use crate::session::naming::ConversationNaming;
use crate::session::restore::{
    ConversationRestore, ReadyAction, ReplayAction, ReplayLoaded, ReplayRead, ResumeStart,
    SettingsSeed,
};
use crate::session::settings::ConversationSettings;
use crate::session::update_readiness::{ConversationWork, Readiness, prepare_stop};
use crate::session::workflows::{RefreshPlan, WorkflowData, WorkflowReader};
use crate::session::{
    AgentKind, Backend, ConversationTitleRequest, OperationError, RecoveryIdentity,
    SettingsOutcome, TaskHistory, TaskHistoryRead,
};
use crate::transcript::TextField;
use crate::transcript::conversation::{ConversationImage, ConversationState, hidden};
use crate::workflow::{WorkflowRefreshRequest, WorkflowRefreshResult, WorkflowRun, WorkflowSource};

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
    ready_defaults: ReadyDefaults,
    goal: Option<GoalStatus>,

    /// An explicit empty snapshot prevents older transcript tasks resurfacing.
    task_list: Option<TaskList>,

    plan_mode: bool,
    command_catalog: Option<Vec<SlashCommandInfo>>,
    skill_catalog: Option<SkillCatalog>,
    conversation: Rc<RefCell<ConversationState>>,
    pending_images: VecDeque<(String, Vec<Arc<ConversationImage>>)>,
    runtime: SessionRuntime,
    delivery: MessageDelivery,
    restore: ConversationRestore,
    naming: ConversationNaming,
    pub controls: ConversationSettings,
    input: SessionInput,
    branch: ConversationBranch,
    commands: CommandQueue,
    children: ChildAgents,
    workflows: WorkflowData,
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
        self.command_catalog = None;
        self.skill_catalog = None;

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

    /// Prepare the read that rebuilds this conversation's child agents, once
    /// per conversation. A repeated `Ready` for the same conversation must not
    /// schedule a second read over children that are already live.
    pub fn begin_task_restoration(&mut self, cwd: Option<&str>) -> Option<TaskHistoryRead> {
        let session_id = self.runtime.backend()?.session_id()?.to_owned();

        if !self.children.claim_restore(&session_id) {
            return None;
        }

        self.runtime.backend_mut()?.begin_task_restoration(cwd)
    }

    pub fn commands(&self) -> &CommandQueue {
        &self.commands
    }

    pub fn clear_commands(&mut self) {
        self.commands.clear();
    }

    pub fn admit_command_while_busy(
        &mut self,
        command: PendingSlashCommand,
        policy: SlashCommandRunPolicy,
    ) -> CommandAdmission {
        self.commands.while_busy(command, policy)
    }

    pub fn goal(&self) -> Option<&GoalStatus> {
        self.goal.as_ref()
    }

    pub fn task_list(&self) -> Option<&TaskList> {
        self.task_list.as_ref()
    }

    pub fn plan_mode(&self) -> bool {
        self.plan_mode
    }

    pub fn set_ready_defaults(&mut self, defaults: ReadyDefaults) {
        self.ready_defaults = defaults;
    }

    /// Keep the images of a sent prompt until its transcript row exists to
    /// take them, matched by the prompt text the row will carry.
    pub fn hold_sent_images(&mut self, text: String, images: Vec<Arc<ConversationImage>>) {
        self.pending_images.push_back((text, images));
    }

    /// The title an unnamed conversation should take from this prompt.
    pub fn title_request(
        &self,
        text: &str,
        fallback: impl FnOnce(&str) -> Option<String>,
    ) -> Option<ConversationTitleRequest> {
        self.naming.request(self.kind, text, fallback)
    }

    /// Record that a prompt claimed the conversation's title, so a failed
    /// asynchronous generation cannot let a later message name it instead.
    pub fn claim_title(&mut self) {
        self.naming.named = true;
    }

    pub fn rename(&mut self, title: &str) {
        self.naming.rename(title);

        self.sync_pending_rename();
    }

    /// A rename made before the harness can address the conversation waits
    /// here and is sent once it can.
    pub fn sync_pending_rename(&mut self) {
        self.naming.sync(self.runtime.backend_mut());
    }

    pub fn begin_resume(&mut self, summary: &SessionSummary, cwd: Option<&str>) -> ResumeStart {
        self.restore
            .begin(&mut self.runtime, self.kind, summary, cwd)
    }

    pub fn replay_loaded(
        &mut self,
        request: ReplayRead,
        cwd: Option<&str>,
        replay: Result<Vec<ReplayTurn>, String>,
    ) -> ReplayLoaded {
        self.restore.loaded(&mut self.runtime, request, cwd, replay)
    }

    /// The transcript content, shared with the view that renders it. The
    /// handle is lent rather than exposed so the session and its view can
    /// never end up holding different conversations.
    pub fn conversation(&self) -> &Rc<RefCell<ConversationState>> {
        &self.conversation
    }

    pub fn runtime(&self) -> &SessionRuntime {
        &self.runtime
    }

    /// Feed one frame from the harness to the session that was started under
    /// `epoch`. `None` means that session is gone and the frame is discarded.
    pub fn process(&mut self, epoch: u64, message: Value) -> Option<Vec<Event>> {
        self.runtime.process(epoch, message)
    }

    pub fn process_exit(&mut self, epoch: u64) -> Option<Vec<Event>> {
        self.runtime.process_exit(epoch)
    }

    /// Take the backend out so its owner can shut it down off this thread.
    pub fn retire(&mut self) -> Option<Backend> {
        self.runtime.retire()
    }

    /// End the session for good and hand back the backend to shut down. A new
    /// epoch is opened first so nothing the old backend still delivers can be
    /// admitted, and the failure releases whatever was waiting on a reply.
    pub fn close(&mut self) -> Option<Backend> {
        self.starting(None);

        let backend = self.runtime.retire();

        self.failed("session closed", true);

        self.clear_conversation();

        backend
    }

    /// The shared host went away underneath a live conversation. Recording
    /// the identity first is what lets a retry continue this conversation
    /// instead of opening an empty one.
    pub fn host_exited(&mut self, profile_name: String, message: String) {
        let identity = self
            .runtime
            .backend()
            .and_then(|backend| backend.recovery_identity());

        self.runtime.reconnect(Some(RecoverySnapshot {
            identity,
            profile_name,
        }));

        self.runtime.recovery_failed(message);
    }

    pub fn wait_for_update(&mut self) {
        self.runtime.wait_for_update();
    }

    pub fn cancel_update_wait(&mut self) -> bool {
        self.runtime.cancel_update_wait()
    }

    /// Bring the conversation to rest before an update replaces its harness.
    /// An open approval is cancelled rather than interrupted, because the
    /// harness is blocked on the answer and would not see an interrupt.
    /// Accepted prompts are published so none is lost with the old process.
    pub fn stop_active_work_for_update(&mut self) {
        if self.input.approval().is_some() {
            self.respond_approval("cancel");
        } else {
            self.runtime.interrupt(None);
        }

        self.prepare_update_stop();

        self.publish_confirmed();

        self.branch.cancel_picker();

        self.conversation.borrow_mut().live.set_compacting(false);

        self.runtime.wait_for_update();
    }

    pub fn suspend_for_update(&mut self) -> (u64, Option<Backend>) {
        self.runtime.suspend_for_update()
    }

    pub fn shutdown_failed(&mut self, epoch: u64, backend: Backend) -> Result<(), Box<Backend>> {
        self.runtime.shutdown_failed(epoch, backend)
    }

    pub fn provider_updating(&mut self) {
        self.runtime.provider_updating();
    }

    pub fn reconnect(&mut self, snapshot: Option<RecoverySnapshot>) {
        self.runtime.reconnect(snapshot);
    }

    pub fn recovery_failed(&mut self, message: String) {
        self.runtime.recovery_failed(message);
    }

    pub fn refresh_background_tasks(&mut self) {
        self.runtime.refresh_background_tasks();
    }

    pub fn interrupt_background_task(&mut self, key: &BackgroundTaskKey) -> bool {
        self.runtime.interrupt_background_task(key)
    }

    /// `None` means no session is running to ask.
    pub fn load_background_task_transcript(
        &mut self,
        key: &BackgroundTaskKey,
        cwd: Option<&str>,
    ) -> Option<Vec<Event>> {
        Some(
            self.runtime
                .backend_mut()?
                .load_background_task_transcript(key, cwd),
        )
    }

    pub fn finish_task_restoration(&mut self, history: TaskHistory) -> Option<Vec<Event>> {
        Some(self.runtime.backend_mut()?.finish_task_restoration(history))
    }

    /// `None` means no session is running to ask.
    pub fn rename_conversation(&mut self, title: &str) -> Option<Result<String, OperationError>> {
        Some(self.runtime.backend_mut()?.rename_conversation(title))
    }

    /// Answers whether a session was running to take the search.
    pub fn search_sessions(&mut self, query: &str) -> bool {
        let Some(backend) = self.runtime.backend_mut() else {
            return false;
        };

        backend.search_sessions(query);

        true
    }

    pub fn request_history(&mut self, scope: SessionScope) {
        if let Some(backend) = self.runtime.backend_mut() {
            backend.request_history(scope);
        }
    }

    pub fn request_more_history(&mut self) {
        if let Some(backend) = self.runtime.backend_mut() {
            backend.request_more_history();
        }
    }

    pub fn respond_team_decision(
        &mut self,
        request: &TeamDecisionRequest,
        accepted: bool,
        explanation: &str,
    ) {
        if let Some(backend) = self.runtime.backend_mut() {
            backend.respond_team_decision(request, accepted, explanation);
        }
    }

    pub fn branch(&self) -> &ConversationBranch {
        &self.branch
    }

    pub fn begin_fork(&mut self, target: Option<PromptTarget>) -> Result<(), BranchError> {
        self.branch.begin_fork(&mut self.runtime, target)
    }

    pub fn fork(&mut self, checkpoint: ForkCheckpoint) -> BranchUpdate {
        self.branch.fork(&mut self.runtime, checkpoint)
    }

    pub fn fork_created(
        &mut self,
        request: ForkRequest,
        result: Result<ClaudeFork, String>,
    ) -> BranchUpdate {
        self.branch
            .fork_created(self.runtime.epoch(), request, result)
    }

    pub fn begin_rewind(
        &mut self,
        cwd: Option<String>,
        target: Option<PromptTarget>,
    ) -> Result<CheckpointRead, BranchError> {
        self.branch.begin_rewind(&self.runtime, cwd, target)
    }

    pub fn checkpoints_loaded(
        &mut self,
        request: CheckpointRead,
        result: Result<Vec<ClaudeCheckpoint>, String>,
    ) -> BranchUpdate {
        self.branch
            .checkpoints_loaded(self.runtime.epoch(), request, result)
    }

    pub fn select_checkpoint(&mut self, checkpoint: ClaudeCheckpoint) -> bool {
        self.branch
            .select_checkpoint(self.runtime.epoch(), checkpoint)
    }

    pub fn rewind(&mut self, action: RewindAction) -> BranchUpdate {
        self.branch.rewind(&mut self.runtime, action)
    }

    pub fn cancel_branch_picker(&mut self) -> bool {
        self.branch.cancel_picker()
    }

    pub fn input(&self) -> &SessionInput {
        &self.input
    }

    /// The answers in progress belong to whoever is typing them. Everything
    /// that moves a question through its lifecycle stays on this controller,
    /// so what is lent here can edit a draft and nothing else.
    pub fn input_mut(&mut self) -> &mut SessionInput {
        &mut self.input
    }

    pub fn can_submit_question(&self, key: QuestionKey) -> bool {
        self.input.can_submit(&self.runtime, key)
    }

    /// The number of the turn in progress, or of the last one once idle.
    pub fn turn(&self) -> u64 {
        self.delivery.turn()
    }

    /// Prompts the harness accepted and has not started, oldest first.
    pub fn queued_prompts(&self) -> &VecDeque<QueuedPrompt> {
        self.delivery.pending()
    }

    /// Ask the harness to drop a prompt it has not started. The row leaves
    /// the queue only once the harness takes the removal, so one it already
    /// claimed stays where the transcript is about to confirm it.
    pub fn withdraw_queued_prompt(&mut self, item_id: &str) -> bool {
        let removed = self
            .runtime
            .backend_mut()
            .is_some_and(|backend| backend.remove_queued_prompt(item_id));

        if removed {
            self.delivery.removed(item_id);
        }

        removed
    }

    pub fn workflows(&self) -> &WorkflowData {
        &self.workflows
    }

    /// The stored record a harness keeps of its workflow runs, where it has
    /// one. A harness that reports runs live has none.
    pub fn workflow_source(&self) -> Option<Arc<dyn WorkflowSource>> {
        self.runtime.backend()?.workflow_source()
    }

    /// The source and conversation to read recorded runs from, once per
    /// conversation: a repeated `Ready` must not read the same record again.
    pub fn begin_workflow_restoration(&mut self) -> Option<(Arc<dyn WorkflowSource>, String)> {
        let source = self.workflow_source()?;
        let session_id = self.runtime.backend()?.session_id()?.to_owned();

        self.workflows
            .claim_restore(&session_id)
            .then_some((source, session_id))
    }

    /// A failed read leaves whatever the live stream reported and releases
    /// the claim, so the next opening of the view retries.
    pub fn merge_restored_workflows(
        &mut self,
        restored: Result<Vec<WorkflowRun>, String>,
    ) -> Vec<Event> {
        let Ok(restored) = restored else {
            self.workflows.forget_restore();

            return Vec::new();
        };

        self.runtime
            .backend_mut()
            .map(|backend| backend.restore_workflows(restored))
            .unwrap_or_default()
    }

    pub fn workflow_refresh_plan(&self, cwd: Option<String>) -> Option<RefreshPlan> {
        self.workflows.refresh_plan(&self.runtime, cwd)
    }

    /// Fold one read in. `None` means the session that asked is gone.
    pub fn apply_workflow_refresh(&mut self, result: WorkflowRefreshResult) -> Option<Vec<Event>> {
        self.workflows.accept_revision(&result);

        Some(self.runtime.backend_mut()?.apply_workflow_refresh(result))
    }

    pub fn mark_open_workflow_availability(&mut self) -> bool {
        self.workflows.mark_open_availability()
    }

    pub fn open_workflow_agent(&mut self, task_id: &str, agent_id: &str) -> WorkflowReader {
        self.workflows.open_agent(task_id, agent_id)
    }

    /// The request a read of one member's conversation is made with, or
    /// nothing when the harness keeps no record and was asked over its
    /// connection instead, where the answer arrives as an ordinary event.
    pub fn read_workflow_agent(
        &mut self,
        task_id: &str,
        agent_id: &str,
    ) -> Option<(Arc<dyn WorkflowSource>, String, WorkflowRefreshRequest)> {
        let Some(source) = self.workflow_source() else {
            self.runtime
                .backend_mut()?
                .request_workflow_agent_transcript(task_id, agent_id);

            return None;
        };

        let session_id = self.runtime.backend()?.session_id()?.to_owned();

        let request = WorkflowRefreshRequest {
            task_id: task_id.to_owned(),
            agent_ids: self.workflows.agent_ids(task_id),
            open_agent: Some(agent_id.to_owned()),
            transcript_revision: None,
        };

        Some((source, session_id, request))
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
            SessionEffect::TurnCompleted { error } => {
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

                SessionEffect::TurnCompleted { error }
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
                self.command_catalog = Some(commands);

                SessionEffect::Commands
            }
            Event::Skills(catalog) => {
                self.skill_catalog = Some(catalog);

                SessionEffect::Skills
            }
            Event::SlashCommandResult { name, outcome } => {
                self.commands.settle(&outcome, self.runtime.status());

                SessionEffect::CommandResult { name, outcome }
            }
            Event::TurnStarted => SessionEffect::TurnStarted {
                opened: self.turn_started(),
            },
            Event::ProviderTurnAccepted { id } => SessionEffect::ProviderTurnAccepted { id },
            Event::TeamDecision(request) => SessionEffect::TeamDecision(request),
            Event::ProviderTurnFinished { id, error } => {
                SessionEffect::ProviderTurnFinished { id, error }
            }
            Event::TurnCompleted { error } => {
                self.turn_completed();

                SessionEffect::TurnCompleted { error }
            }
            Event::TurnOutputTokensUpdated(tokens) => {
                let mut conversation = self.conversation.borrow_mut();

                if conversation.live.set_output_tokens(tokens) {
                    conversation.changed_turn(self.delivery.turn());
                }

                SessionEffect::Changed
            }
            Event::GenerationCompleted(sample) => {
                let mut conversation = self.conversation.borrow_mut();

                if conversation.live.is_working() && conversation.generation_stats.record(sample) {
                    SessionEffect::Changed
                } else {
                    SessionEffect::Unchanged
                }
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
            Event::HostExited { message } => {
                let failure = self.failed(&message, true);

                SessionEffect::Error {
                    message,
                    fatal: true,
                    failure,
                }
            }
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

        let approval = self
            .runtime
            .backend_mut()
            .and_then(|backend| self.controls.apply_approval(backend, reported_approval));

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
    fn finish_ready(&mut self, settings: ThreadSettings) -> Option<SettingsOutcome> {
        self.input.restore(&mut self.runtime);

        let defaults = self.ready_defaults.clone();

        self.controls.ready(
            self.kind,
            settings,
            defaults.stored.as_ref(),
            defaults.model.as_deref(),
            defaults.effort.as_deref(),
        );

        let selection = self
            .runtime
            .backend_mut()
            .and_then(|backend| self.controls.apply_model(backend));

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

    pub fn apply_model_selection(&mut self) -> Option<SettingsOutcome> {
        self.controls.apply_model(self.runtime.backend_mut()?)
    }

    pub fn select_agent_preset(&mut self, preset: String) -> Option<SettingsOutcome> {
        if self.controls.settings.agent_preset.as_deref() == Some(&preset) {
            return None;
        }

        let outcome = self.runtime.backend_mut()?.select_agent_preset(&preset);

        match outcome {
            SettingsOutcome::Effective | SettingsOutcome::RidesNextSubmission => {
                self.controls.settings.agent_preset = Some(preset);
            }
            SettingsOutcome::Refused { .. } => {}
        }

        Some(outcome)
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
    fn turn_completed(&mut self) {
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

/// Tests arrange a part's state directly and assert on it afterwards. The
/// application reaches the same parts only through the operations above, so
/// these stay out of a build without the test feature.
#[cfg(any(test, feature = "test-support"))]
impl SessionController {
    /// A fixture opens its pane as a harness that needs no subprocess and
    /// then plays another one through the test backend.
    pub fn set_kind(&mut self, kind: AgentKind) {
        self.kind = kind;
    }

    pub fn restore_parts(&mut self) -> (&mut ConversationRestore, &mut SessionRuntime) {
        (&mut self.restore, &mut self.runtime)
    }

    pub fn runtime_mut(&mut self) -> &mut SessionRuntime {
        &mut self.runtime
    }

    pub fn branch_parts(&mut self) -> (&mut ConversationBranch, &mut SessionRuntime) {
        (&mut self.branch, &mut self.runtime)
    }

    pub fn delivery(&self) -> &MessageDelivery {
        &self.delivery
    }

    pub fn naming_mut(&mut self) -> &mut ConversationNaming {
        &mut self.naming
    }

    pub fn commands_mut(&mut self) -> &mut CommandQueue {
        &mut self.commands
    }
}

pub struct SessionReady {
    pub branch: Option<BranchCompletion>,
    pub replaced: bool,
    pub selection: Option<SettingsOutcome>,

    /// The outcome of sending a remembered permission preset to a harness
    /// that pinned its own default into the new conversation.
    pub approval: Option<SettingsOutcome>,
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
    Commands,
    Skills,
    CommandResult {
        name: String,
        outcome: SlashCommandOutcome,
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
    Error {
        message: String,
        fatal: bool,
        failure: SessionFailure,
    },
    EffortRejected {
        message: String,
    },
}
