#[cfg(test)]
#[path = "agent_tests.rs"]
mod agent_tests;

use std::sync::Arc;
use std::time::Instant;

use anyhow::anyhow;
use nmt_agent::catalog::{SlashRefusal, SlashRoute};
use nmt_agent::chat::{
    Question, QuestionInput, SessionScope, SkillReference, SlashCommandOutcome,
    SlashCommandRunPolicy,
};
use nmt_agent::session::OperationError;
use nmt_agent::session::command::{
    AdmitSlashCommand, AgentCommand, AnswerQuestion, ApplyModelSelection, Interrupt,
    NEW_CONVERSATION_METHOD, Prompt, RenameConversation, RespondApproval, RunSlashCommand,
    SubmitPrompt, SubmitRefusal, WithdrawQueuedPrompt,
};
use nmt_agent::session::commands::{CommandAdmission, PendingSlashCommand};
use nmt_agent::session::controller::SubmissionBlock;
use nmt_agent::session::delivery::RecoverablePrompt;
use nmt_agent::session::history::{HISTORY_METHOD, HistoryStep};
use nmt_agent::session::input::{
    ApprovalOutcome, QuestionAction, QuestionDraft, QuestionStatus, Submission,
};
use nmt_agent::session::lifecycle::{InterruptOutcome, Status};
use nmt_agent::session::view::{AgentView, DraftAnswers, ViewOp, ViewSlot};
use nmt_platform::runtime;
use nmt_remote::connection::{AgentLink, AgentUpdate, RemoteHost};
use parking_lot::Mutex;
use serde_json::Value;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::task::AbortHandle;
use tracing::warn;

use crate::commands::{bound_skill, refusal_text, route_line};
use crate::error::CoreError;
use crate::records::{AgentEntry, AgentState, QuestionAnswer, ViewEnd, agent_entry, agent_state};

/// The longest title a prompt gives an unnamed conversation, matching what
/// the desktop takes from its own prompts.
const TITLE_CHARS: usize = 60;

/// Told when an agent view changed. Called on the core's threads.
#[uniffi::export(with_foreign)]
pub trait AgentObserver: Send + Sync {
    /// The view changed. `transcript_from` is the first transcript entry
    /// that changed, when any did; everything before it is unchanged.
    fn changed(&self, transcript_from: Option<u32>);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum ApprovalDecision {
    Accept,
    /// Accept, and let the agent do the same again this session without
    /// asking.
    AcceptForSession,
    Decline,
    /// Decline and stop the turn.
    Cancel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum ApprovalResult {
    /// There was no request to answer any more.
    Ignored,
    Rejected,
    Waiting,
    Settled,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum SubmitResult {
    /// The agent took the message; `started_turn` is false when it queued
    /// behind the running turn.
    Accepted {
        started_turn: bool,
    },
    /// The agent is still starting.
    NotReady,
    /// An answer to a question is still being sent.
    AnswerPending,
    /// A rewind or fork is changing the conversation.
    ConversationChanging,
    /// A slash command is starting.
    CommandStarting,
    Rejected {
        message: String,
    },
    /// The line was a command the agent took; what it does shows in the
    /// transcript.
    CommandStarted,
    /// The line was a command that finished at once, with what it reported.
    CommandFinished {
        message: Option<String>,
    },
    /// The line was a command that runs once the running turn ends;
    /// `count` commands wait now.
    CommandQueued {
        count: u32,
    },
    /// The line started a new conversation in place of this one.
    ConversationReplaced,
    /// Open the recent conversation picker without sending a message.
    ShowRecentSessions,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum AnswerResult {
    /// The batch no longer waits, or the conversation is changing.
    Ignored,
    /// A question has no answer yet.
    Incomplete,
    /// The agent took the answer and still waits on other questions.
    Waiting,
    Settled,
    /// The answer did not reach the agent; the batch says why.
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct InterruptResult {
    pub accepted: bool,

    /// The message the interrupt took back before the agent answered it,
    /// for the composer to show again.
    pub restored_text: Option<String>,
}

/// A view of one agent session on a host. The host keeps the session's
/// controller; this holds the projection it publishes and sends commands.
#[derive(uniffi::Object)]
pub struct AgentHandle {
    host: Arc<RemoteHost>,
    link: Arc<AgentLink>,
    replica: Arc<Mutex<Replica>>,
    tasks: Vec<AbortHandle>,
}

#[derive(Default)]
struct Replica {
    view: Option<AgentView>,

    /// When the running turn started, on this device's clock.
    turn_started: Option<Instant>,

    /// The host went out of reach for good, or stopped trusting the device.
    unreachable: bool,
}

impl Replica {
    /// Apply one update and return the first transcript entry it changed.
    fn apply(&mut self, update: AgentUpdate) -> Option<usize> {
        match update {
            AgentUpdate::Snapshot(value) => match serde_json::from_value::<AgentView>(value) {
                Ok(view) => {
                    self.turn_started = view.slots.status.live.started.instant();
                    self.view = Some(view);

                    Some(0)
                }
                Err(error) => {
                    warn!(%error, "ignoring an agent snapshot this build cannot read");

                    None
                }
            },
            AgentUpdate::Ops(value) => {
                let ops = match serde_json::from_value::<Vec<ViewOp>>(value) {
                    Ok(ops) => ops,
                    Err(error) => {
                        warn!(%error, "ignoring agent changes this build cannot read");

                        return None;
                    }
                };

                // Changes before the snapshot describe a view this device
                // never had; the snapshot that follows replaces them.
                let view = self.view.as_mut()?;

                let mut from = None;

                for op in ops {
                    match &op {
                        ViewOp::Splice { from: at, .. } => {
                            from = Some(from.map_or(*at, |known: usize| known.min(*at)));
                        }
                        ViewOp::Slot { slot } => {
                            if let ViewSlot::Status(status) = slot.as_ref() {
                                self.turn_started = status.live.started.instant();
                            }
                        }
                    }

                    view.apply(op);
                }

                from
            }
            AgentUpdate::Ended => {
                self.unreachable = true;

                None
            }
        }
    }
}

impl AgentHandle {
    pub(crate) fn attach(
        host: Arc<RemoteHost>,
        session: String,
        observer: Arc<dyn AgentObserver>,
    ) -> Arc<Self> {
        let (link, updates) = host.agent_view(session.clone());
        let replica = Arc::new(Mutex::new(Replica::default()));

        let follow = runtime()
            .spawn(follow(updates, Arc::clone(&replica), Arc::clone(&observer)))
            .abort_handle();

        let ended = runtime()
            .spawn(watch_ended(Arc::clone(&host), observer))
            .abort_handle();

        Arc::new(Self {
            host,
            link: Arc::new(link),
            replica,
            tasks: vec![follow, ended],
        })
    }

    async fn send_message(
        &self,
        text: String,
        skill: Option<SkillReference>,
    ) -> Result<SubmitResult, CoreError> {
        let prompt = Prompt {
            fallback_title: fallback_title(&text),
            title_text: text.clone(),
            skill: skill.clone(),
            images: Vec::new(),
            image_paths: Vec::new(),
            recoverable: Some(RecoverablePrompt {
                text: text.clone(),
                response_annotations: Vec::new(),
                skill,
            }),
            text,
        };

        Ok(match self.run(SubmitPrompt(prompt)).await? {
            Ok(submitted) => SubmitResult::Accepted {
                started_turn: submitted.started_turn,
            },
            Err(SubmitRefusal::NotReady) => SubmitResult::NotReady,
            Err(SubmitRefusal::Blocked(SubmissionBlock::QuestionResponse)) => {
                SubmitResult::AnswerPending
            }
            Err(SubmitRefusal::Blocked(SubmissionBlock::ConversationChange)) => {
                SubmitResult::ConversationChanging
            }
            Err(SubmitRefusal::Blocked(SubmissionBlock::CommandStarting)) => {
                SubmitResult::CommandStarting
            }
            Err(SubmitRefusal::Rejected { message }) => SubmitResult::Rejected { message },
        })
    }

    /// Run a harness command, or have it wait by its policy while a turn
    /// holds the session, as the desktop composer does.
    async fn run_command(
        &self,
        command: PendingSlashCommand,
        policy: SlashCommandRunPolicy,
        busy: bool,
    ) -> Result<SubmitResult, CoreError> {
        let command = match busy {
            false => command,
            true => match self.run(AdmitSlashCommand { command, policy }).await? {
                CommandAdmission::Execute(command) => command,
                CommandAdmission::Queued { count, .. } => {
                    return Ok(SubmitResult::CommandQueued {
                        count: count as u32,
                    });
                }
                CommandAdmission::Busy { name } => {
                    return Ok(SubmitResult::Rejected {
                        message: refusal_text(SlashRefusal::IdleOnly(name)),
                    });
                }
            },
        };

        Ok(match self.run(RunSlashCommand { command }).await? {
            SlashCommandOutcome::Accepted => SubmitResult::CommandStarted,
            SlashCommandOutcome::Completed { message, .. } => {
                SubmitResult::CommandFinished { message }
            }
            SlashCommandOutcome::Rejected { message } => SubmitResult::Rejected { message },
            SlashCommandOutcome::NotReady => SubmitResult::NotReady,
        })
    }

    /// Settle the pending batch `id`: with `answers`, or as skipped when
    /// there are none. Answers are checked the way the desktop checks its
    /// own before anything is sent.
    async fn settle_questions(
        &self,
        id: &str,
        answers: Option<Vec<QuestionAnswer>>,
    ) -> Result<AnswerResult, CoreError> {
        let draft = self.replica.lock().view.as_ref().and_then(|view| {
            view.slots
                .pending
                .drafts
                .iter()
                .find(|draft| draft.id == id && draft.status == QuestionStatus::Pending)
                .cloned()
        });

        let Some(draft) = draft else {
            return Ok(AnswerResult::Ignored);
        };

        let key = draft.key;

        let (action, answers) = match answers {
            None => (
                QuestionAction::Skip,
                DraftAnswers {
                    selected: draft.selected.clone(),
                    text: draft.text.clone(),
                    custom: draft.custom.clone(),
                },
            ),
            Some(answers) => {
                let Some(answers) = draft_answers(&draft.questions, answers) else {
                    return Ok(AnswerResult::Incomplete);
                };

                let mut check = QuestionDraft::from_view(draft);

                check.set_answers(answers.clone());

                if !check.is_complete() {
                    return Ok(AnswerResult::Incomplete);
                }

                (QuestionAction::Answer, answers)
            }
        };

        Ok(
            match self
                .run(AnswerQuestion {
                    key,
                    action,
                    answers,
                })
                .await?
            {
                Submission::Ignored => AnswerResult::Ignored,
                Submission::Waiting => AnswerResult::Waiting,
                Submission::Settled { .. } => AnswerResult::Settled,
                Submission::Failed => AnswerResult::Failed,
            },
        )
    }

    async fn run<C: AgentCommand>(&self, command: C) -> Result<C::Outcome, CoreError> {
        let link = Arc::clone(&self.link);
        let params = serde_json::to_value(&command)?;

        let value = runtime()
            .spawn(async move { link.call(C::METHOD, params).await })
            .await??;

        Ok(serde_json::from_value(value)?)
    }

    async fn history_step(&self, step: HistoryStep) -> Result<(), CoreError> {
        let link = Arc::clone(&self.link);
        let params = serde_json::to_value(step)?;

        let value = runtime()
            .spawn(async move { link.call(HISTORY_METHOD, params).await })
            .await??;

        serde_json::from_value::<Result<(), String>>(value)?
            .map_err(|message| CoreError::Failed { message })
    }
}

#[uniffi::export]
impl AgentHandle {
    pub fn session(&self) -> String {
        self.link.session().to_owned()
    }

    /// Transcript entries from `from` to the end.
    pub fn entries(&self, from: u32) -> Vec<AgentEntry> {
        let replica = self.replica.lock();

        let Some(view) = &replica.view else {
            return Vec::new();
        };

        view.transcript
            .iter()
            .enumerate()
            .skip(from as usize)
            .map(|(index, entry)| agent_entry(index, entry))
            .collect()
    }

    pub fn state(&self) -> AgentState {
        let replica = self.replica.lock();

        let ended = match (replica.unreachable, self.host.ended(self.link.session())) {
            (true, _) => Some(ViewEnd::Unreachable),
            (false, reason) => reason.map(ViewEnd::from),
        };

        agent_state(replica.view.as_ref(), replica.turn_started, ended)
    }

    /// Take the session back after the person at the host took it.
    pub fn take_control(&self) {
        self.host.reconnect(self.link.session());
    }

    /// Send what the person typed: a slash line runs as the command it
    /// names, anything else goes to the agent as a message. `skill_path` is
    /// the skill picked for a message that starts with its `$name`.
    pub async fn submit(
        &self,
        text: String,
        skill_path: Option<String>,
    ) -> Result<SubmitResult, CoreError> {
        let (route, skill) = {
            let replica = self.replica.lock();

            match &replica.view {
                Some(view) => (
                    route_line(&text, view),
                    bound_skill(&text, skill_path.as_deref(), view),
                ),
                None => (None, None),
            }
        };

        let Some((route, busy)) = route else {
            return self.send_message(text, skill).await;
        };

        Ok(match route {
            SlashRoute::Prompt => return self.send_message(text, None).await,
            SlashRoute::Refused(refusal) => SubmitResult::Rejected {
                message: refusal_text(refusal),
            },
            SlashRoute::NewConversation => {
                self.new_conversation().await?;

                SubmitResult::ConversationReplaced
            }
            SlashRoute::Resume => SubmitResult::ShowRecentSessions,
            SlashRoute::Rename(title) => {
                self.rename(title).await?;

                SubmitResult::CommandFinished { message: None }
            }
            SlashRoute::Backend { command, policy } => {
                return self.run_command(command, policy, busy).await;
            }
            // The phone's catalog leaves out every command that routes here.
            _ => SubmitResult::Rejected {
                message: "This command works on the computer only.".to_owned(),
            },
        })
    }

    /// Replace the conversation with a new one, which restarts the agent on
    /// the host. The host refuses while a turn is running.
    pub async fn new_conversation(&self) -> Result<(), CoreError> {
        let link = Arc::clone(&self.link);

        let value = runtime()
            .spawn(async move { link.call(NEW_CONVERSATION_METHOD, Value::Null).await })
            .await??;

        serde_json::from_value::<Result<(), String>>(value)?
            .map_err(|message| CoreError::Failed { message })
    }

    /// Request the host's recent conversations for this working directory.
    /// Results and loading status arrive through the agent state updates.
    pub async fn request_recent_sessions(&self) -> Result<(), CoreError> {
        if !self
            .replica
            .lock()
            .view
            .as_ref()
            .is_some_and(|view| matches!(view.slots.status.status, Status::Idle | Status::Running))
        {
            return Err(anyhow!("the agent is not ready to list conversations").into());
        }

        self.history_step(HistoryStep::List(SessionScope::CurrentDirectory))
            .await
    }

    /// Resume a conversation from the host's last published history list.
    pub async fn resume_recent_session(&self, id: String) -> Result<(), CoreError> {
        let summary = self
            .replica
            .lock()
            .view
            .as_ref()
            .and_then(|view| view.slots.history.iter().find(|session| session.id == id))
            .cloned()
            .ok_or_else(|| anyhow!("the conversation is no longer in the recent list"))?;

        self.history_step(HistoryStep::Resume(summary)).await
    }

    /// Answer the question batch `id`, one answer per question.
    pub async fn answer_questions(
        &self,
        id: String,
        answers: Vec<QuestionAnswer>,
    ) -> Result<AnswerResult, CoreError> {
        self.settle_questions(&id, Some(answers)).await
    }

    /// Tell the agent the person will not answer the batch `id`.
    pub async fn skip_questions(&self, id: String) -> Result<AnswerResult, CoreError> {
        self.settle_questions(&id, None).await
    }

    pub async fn interrupt(&self) -> Result<InterruptResult, CoreError> {
        let interruption = self.run(Interrupt).await?;

        Ok(InterruptResult {
            accepted: interruption.outcome == InterruptOutcome::Accepted,
            restored_text: interruption.prompt.map(|(_, prompt)| prompt.text),
        })
    }

    pub async fn respond_approval(
        &self,
        decision: ApprovalDecision,
    ) -> Result<ApprovalResult, CoreError> {
        // The decision names the agent protocols use, which the host passes
        // through to the agent.
        let decision = match decision {
            ApprovalDecision::Accept => "accept",
            ApprovalDecision::AcceptForSession => "acceptForSession",
            ApprovalDecision::Decline => "decline",
            ApprovalDecision::Cancel => "cancel",
        };

        let outcome = self
            .run(RespondApproval {
                decision: decision.to_owned(),
            })
            .await?;

        Ok(match outcome {
            ApprovalOutcome::Ignored => ApprovalResult::Ignored,
            ApprovalOutcome::Rejected => ApprovalResult::Rejected,
            ApprovalOutcome::Waiting => ApprovalResult::Waiting,
            ApprovalOutcome::Settled => ApprovalResult::Settled,
        })
    }

    /// Take a queued message back before the agent reads it. False when the
    /// agent already took it.
    pub async fn withdraw(&self, id: String) -> Result<bool, CoreError> {
        self.run(WithdrawQueuedPrompt { item_id: id }).await
    }

    /// Run the next turns on `model` with `effort`.
    pub async fn select_model(
        &self,
        model: String,
        effort: Option<String>,
    ) -> Result<(), CoreError> {
        let mut settings = self
            .replica
            .lock()
            .view
            .as_ref()
            .map(|view| view.slots.settings.settings.clone())
            .unwrap_or_default();

        settings.model = Some(model);
        settings.effort = effort;

        self.run(ApplyModelSelection { settings }).await?;

        Ok(())
    }

    pub async fn rename(&self, title: String) -> Result<(), CoreError> {
        match self.run(RenameConversation { title }).await? {
            None | Some(Ok(_)) => Ok(()),
            Some(Err(OperationError::Failed(message))) => {
                Err(anyhow!(message).context("renaming failed").into())
            }
            Some(Err(OperationError::Unsupported(_))) => {
                Err(anyhow!("this agent cannot rename its conversations").into())
            }
        }
    }
}

impl Drop for AgentHandle {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

/// Apply the host's updates as they arrive, telling the observer once per
/// burst instead of once per message.
async fn follow(
    mut updates: UnboundedReceiver<AgentUpdate>,
    replica: Arc<Mutex<Replica>>,
    observer: Arc<dyn AgentObserver>,
) {
    while let Some(update) = updates.recv().await {
        let from = {
            let mut replica = replica.lock();
            let mut from = replica.apply(update);

            while let Ok(update) = updates.try_recv() {
                if let Some(at) = replica.apply(update) {
                    from = Some(from.map_or(at, |known| known.min(at)));
                }
            }

            from
        };

        observer.changed(from.map(|from| from as u32));
    }
}

/// The host ending or restoring this device's views changes what the
/// screen shows, though nothing in the view itself changed.
async fn watch_ended(host: Arc<RemoteHost>, observer: Arc<dyn AgentObserver>) {
    let mut changes = host.ended_changes();

    while changes.changed().await.is_ok() {
        observer.changed(None);
    }
}

/// The host's form of one answer per question, or `None` when the count
/// does not match the batch. Typed text replaces the picks only where the
/// question takes text, and a single-choice question keeps its first pick.
fn draft_answers(questions: &[Question], answers: Vec<QuestionAnswer>) -> Option<DraftAnswers> {
    if answers.len() != questions.len() {
        return None;
    }

    let mut draft = DraftAnswers {
        selected: Vec::new(),
        text: Vec::new(),
        custom: Vec::new(),
    };

    for (question, answer) in questions.iter().zip(answers) {
        let mut picks: Vec<usize> = answer
            .selected
            .into_iter()
            .map(|pick| pick as usize)
            .filter(|pick| *pick < question.options.len())
            .collect();

        if question.multi_select {
            picks.sort_unstable();
            picks.dedup();
        } else {
            picks.truncate(1);
        }

        let typed = answer
            .text
            .filter(|_| question.input != QuestionInput::SelectionOnly);

        draft.selected.push(picks);
        draft.custom.push(typed.is_some());
        draft.text.push(typed.unwrap_or_default());
    }

    Some(draft)
}

/// The name a harness without its own title generation gives the
/// conversation: the first line typed, unless it is a slash command.
fn fallback_title(text: &str) -> Option<String> {
    let line = text.lines().find(|line| !line.trim().is_empty())?.trim();

    (!line.starts_with('/')).then(|| line.chars().take(TITLE_CHARS).collect())
}
