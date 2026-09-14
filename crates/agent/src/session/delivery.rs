//! User-message admission, confirmation, and recovery independent of presentation.

#[cfg(test)]
#[path = "delivery_tests.rs"]
mod delivery_tests;

use std::collections::VecDeque;

use crate::chat::{QueuedPrompt, SendOutcome, SkillReference};
use crate::session::AgentKind;

/// What a backend does with a prompt submitted while a turn is running.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum QueuedPromptDelivery {
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

/// Text and bindings returned when an unanswered prompt is interrupted.
#[derive(Debug)]
pub struct RecoverablePrompt {
    pub text: String,
    pub response_annotations: Vec<String>,
    pub skill: Option<SkillReference>,
}

/// Admission result; only a new turn publishes its message immediately.
#[derive(Debug, PartialEq, Eq)]
pub enum Submission {
    Started { text: String },
    Queued,
    NotReady,
    Rejected { message: String },
}

/// Owns pending messages and recovery eligibility for one conversation.
/// Confirmed messages are consumed immediately after each transition with
/// `pop_confirmed`, retaining the queue allocation between ordinary turns.
pub struct MessageDelivery {
    policy: QueuedPromptDelivery,
    turn: u64,
    active: bool,
    unanswered: Option<(u64, RecoverablePrompt)>,
    pending: VecDeque<QueuedPrompt>,
    published_prompt: Option<String>,
    confirmed: usize,
}

impl MessageDelivery {
    pub fn new(kind: AgentKind) -> Self {
        Self {
            policy: match kind {
                AgentKind::Codex => QueuedPromptDelivery::RunningTurn,
                AgentKind::Claude => QueuedPromptDelivery::FollowingTurn,
                AgentKind::DeepSeek => QueuedPromptDelivery::PendingInbox,
            },
            turn: 0,
            active: false,
            unanswered: None,
            pending: VecDeque::new(),
            published_prompt: None,
            confirmed: 0,
        }
    }

    #[inline]
    pub fn turn(&self) -> u64 {
        self.turn
    }

    #[inline]
    pub fn is_active(&self) -> bool {
        self.active
    }

    #[inline]
    pub fn pending(&self) -> &VecDeque<QueuedPrompt> {
        &self.pending
    }

    /// A refusal changes no delivery state and never builds recovery data.
    /// Recovery is retained only for the prompt that starts a new turn.
    pub fn submit(
        &mut self,
        outcome: SendOutcome,
        text: String,
        recovery: impl FnOnce() -> Option<RecoverablePrompt>,
    ) -> Submission {
        match outcome {
            SendOutcome::NotReady => Submission::NotReady,
            SendOutcome::Rejected { message } => Submission::Rejected { message },
            SendOutcome::Steered => {
                self.pending.push_back(QueuedPrompt::local(text));

                Submission::Queued
            }
            SendOutcome::StartedTurn => {
                self.begin_turn();

                if self.policy == QueuedPromptDelivery::PendingInbox {
                    self.published_prompt = Some(text.clone());
                }

                self.unanswered = recovery().map(|prompt| (self.turn, prompt));

                Submission::Started { text }
            }
        }
    }

    /// Commands and accepted question answers can open a turn without a prompt.
    pub(crate) fn begin_turn(&mut self) -> u64 {
        self.turn += 1;
        self.active = true;

        self.turn
    }

    /// A submitted turn already has a number; a provider-opened turn does not.
    pub(crate) fn provider_started(&mut self) -> bool {
        if self.active {
            return false;
        }

        self.begin_turn();

        if self.policy == QueuedPromptDelivery::FollowingTurn {
            self.confirmed = self.pending.len();
        }

        true
    }

    /// Restored turns reserve numbers without starting live work.
    pub(crate) fn replay_turn(&mut self) -> u64 {
        self.turn += 1;

        self.turn
    }

    /// Only visible activity answers the active prompt. Hidden provider items
    /// do not revoke the user's ability to recover an unanswered submission.
    #[inline]
    pub(crate) fn visible_output(&mut self) {
        if self
            .unanswered
            .as_ref()
            .is_some_and(|(turn, _)| *turn == self.turn)
        {
            self.unanswered = None;
        }
    }

    pub(crate) fn take_interrupted_prompt(&mut self) -> Option<(u64, RecoverablePrompt)> {
        let prompt = self
            .unanswered
            .take()
            .filter(|(turn, _)| self.active && *turn == self.turn)?;

        // Recovering an unanswered submission discards its provisional turn.
        // A provider that continues anyway opens a new turn on its next start.
        self.active = false;

        Some(prompt)
    }

    pub(crate) fn agent_message(&mut self) {
        if self.policy == QueuedPromptDelivery::RunningTurn {
            self.confirmed = self.pending.len();
        }
    }

    pub fn completed(&mut self) {
        self.active = false;
        self.unanswered = None;

        self.agent_message();
    }

    /// A dead session cannot claim further work, so all remaining accepted
    /// prompts are returned for publication before the exit diagnostic.
    pub fn exited(&mut self) {
        self.active = false;
        self.unanswered = None;
        self.confirmed = self.pending.len();
    }

    /// Publish accepted work before an update retires its provider, while
    /// leaving the active turn open until its completion arrives.
    pub(crate) fn stopping_for_update(&mut self) {
        self.confirmed = self.pending.len();
    }

    pub(crate) fn start_failed(&mut self) {
        self.active = false;
        self.unanswered = None;

        self.pending.clear();

        self.confirmed = 0;
    }

    /// Return confirmed text by ownership without replacing the queue storage.
    #[inline]
    pub(crate) fn pop_confirmed(&mut self) -> Option<String> {
        if self.confirmed == 0 {
            return None;
        }

        self.confirmed -= 1;

        self.pending.pop_front().map(|prompt| prompt.text)
    }

    /// Only the oldest matching pending prompt can be acknowledged by an echo.
    /// Removing it here also prevents a later snapshot from publishing it twice.
    pub fn echoed(&mut self, text: &str) -> Option<String> {
        if !self
            .pending
            .front()
            .is_some_and(|prompt| prompt.text == text)
        {
            return None;
        }

        self.confirmed = self.confirmed.saturating_sub(1);

        self.pending.pop_front().map(|prompt| prompt.text)
    }

    /// Replace optimistic entries with the provider's list and return prompts
    /// it has claimed. Text matching is necessary until the provider assigns ids.
    pub fn snapshot(&mut self, mut prompts: Vec<QueuedPrompt>) -> Vec<String> {
        if let Some(drawn) = self.published_prompt.take() {
            let before = prompts.len();

            prompts.retain(|prompt| prompt.text != drawn);

            if prompts.len() != before {
                self.published_prompt = Some(drawn);
            }
        }

        let claimed = self
            .pending
            .drain(..)
            .filter(|held| !prompts.iter().any(|pending| pending.text == held.text))
            .map(|held| held.text)
            .collect();

        self.pending = prompts.into();
        self.confirmed = 0;

        claimed
    }

    /// Called only after the provider accepts removal; rejected requests leave
    /// the pending list unchanged.
    pub fn removed(&mut self, id: &str) {
        self.pending
            .retain(|prompt| prompt.id.as_deref() != Some(id));
    }

    pub fn reset(&mut self) {
        self.turn = 0;
        self.active = false;
        self.unanswered = None;

        self.pending.clear();

        self.published_prompt = None;
        self.confirmed = 0;
    }
}
