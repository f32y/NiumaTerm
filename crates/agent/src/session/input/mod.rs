//! User input requests, response admission, and reconnect recovery.

pub use crate::session::input::approval::ApprovalOutcome;
pub use crate::session::input::draft::{QuestionDraft, QuestionStatus};

mod approval;
mod draft;

#[cfg(test)]
mod tests;

use std::time::Instant;

use crate::chat::{
    Question, QuestionMode, QuestionRequest, QuestionResolution, QuestionResponse, ThreadSettings,
};
use crate::session::SessionRuntime;
use crate::session::lifecycle::Status;

/// Distinguishes a draft from a later request reusing its provider ID or list position.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct QuestionKey {
    index: usize,
    generation: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum QuestionError {
    Disconnected,
    Rejected(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuestionAction {
    Answer,
    Skip,
    Timeout,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Submission {
    Ignored,
    Waiting,
    Settled,
    Failed,
}

/// Accepted completion data; the caller owns transcript and notification updates.
pub struct QuestionCompletion {
    pub message: Option<String>,
    pub started_turn: bool,
    pub waiting_finished: bool,
}

/// Retains retryable drafts across startup, then restores them against the ready backend.
/// Incoming provider events must first pass the runtime's epoch admission check.
#[derive(Default)]
pub struct SessionInput {
    epoch: u64,
    sequence: u64,
    disconnected: bool,
    approval: Option<approval::Approval>,
    batches: Vec<QuestionDraft>,
}

impl SessionInput {
    pub fn batches(&self) -> &[QuestionDraft] {
        &self.batches
    }

    pub fn draft(&self, key: QuestionKey) -> Option<&QuestionDraft> {
        self.batches.get(key.index).filter(|draft| draft.key == key)
    }

    pub fn draft_mut(&mut self, key: QuestionKey) -> Option<&mut QuestionDraft> {
        self.batches
            .get_mut(key.index)
            .filter(|draft| draft.key == key)
    }

    pub fn has_submission(&self) -> bool {
        self.batches
            .iter()
            .any(|draft| draft.status == QuestionStatus::Submitting)
    }

    pub fn pending_count(&self) -> usize {
        self.batches.iter().filter(|draft| draft.pending()).count()
    }

    pub fn waiting(&self) -> bool {
        self.approval.is_some()
            || self
                .batches
                .iter()
                .any(|draft| draft.pending() && draft.mode != QuestionMode::Async)
    }

    fn next_key(&mut self, index: usize) -> QuestionKey {
        self.sequence += 1;

        QuestionKey {
            index,
            generation: self.sequence,
        }
    }

    pub fn receive(&mut self, runtime: &SessionRuntime, request: QuestionRequest) -> Option<usize> {
        let existing = self.batches.iter().position(|draft| draft.id == request.id);

        if let Some(index) = existing
            && self.batches[index].status != QuestionStatus::History
        {
            return None;
        }

        let mut draft: QuestionDraft = request.into();

        draft.identity = runtime
            .backend()
            .and_then(|backend| backend.recovery_identity());

        Some(self.insert(draft, existing))
    }

    fn insert(&mut self, mut draft: QuestionDraft, existing: Option<usize>) -> usize {
        draft.key = self.next_key(existing.unwrap_or(self.batches.len()));

        if let Some(index) = existing {
            self.batches[index] = draft;

            index
        } else {
            self.batches.push(draft);

            self.batches.len() - 1
        }
    }

    pub fn history(&mut self, item_id: &str, questions: Vec<Question>) -> usize {
        let id = format!("message:{item_id}");

        if let Some(index) = self.batches.iter().position(|draft| draft.id == id) {
            return index;
        }

        let mut draft = QuestionDraft::new(id, questions);

        draft.mode = QuestionMode::Async;
        draft.status = QuestionStatus::History;
        draft.key = self.next_key(self.batches.len());
        self.batches.push(draft);

        self.batches.len() - 1
    }

    pub fn can_submit(&self, runtime: &SessionRuntime, key: QuestionKey) -> bool {
        self.epoch == runtime.epoch()
            && !self.disconnected
            && matches!(runtime.status(), Status::Idle | Status::Running)
            && runtime.update_suspension().is_none()
            && !self.has_submission()
            && self
                .batches
                .get(key.index)
                .is_some_and(|draft| draft.key == key && draft.status == QuestionStatus::Pending)
    }

    pub fn submit(
        &mut self,
        runtime: &mut SessionRuntime,
        key: QuestionKey,
        action: QuestionAction,
        settings: &ThreadSettings,
        now: Instant,
    ) -> Submission {
        if !self.can_submit(runtime, key) {
            return Submission::Ignored;
        }

        let Some(draft) = self.draft_mut(key) else {
            return Submission::Ignored;
        };

        let answers = match action {
            QuestionAction::Answer if draft.is_complete() => Some(draft.answers()),
            QuestionAction::Answer => return Submission::Ignored,
            QuestionAction::Skip => None,

            QuestionAction::Timeout
                if draft
                    .auto_resolve_remaining(now)
                    .is_some_and(|left| left.is_zero()) =>
            {
                None
            }

            QuestionAction::Timeout => return Submission::Ignored,
        };

        let Some(backend) = runtime.backend_mut() else {
            return Submission::Ignored;
        };

        let result = backend.respond_input(&draft.id, answers, settings);

        draft.touch();

        match result {
            Ok(QuestionResponse::Settled) => {
                draft.settle(if action == QuestionAction::Answer {
                    QuestionStatus::Submitted
                } else {
                    QuestionStatus::Skipped
                });

                Submission::Settled
            }

            Ok(QuestionResponse::Pending) => {
                draft.error = None;
                draft.status = QuestionStatus::Submitting;

                Submission::Waiting
            }

            Err(message) => {
                draft.error = Some(QuestionError::Rejected(message));

                Submission::Failed
            }
        }
    }

    pub fn resolve(
        &mut self,
        epoch: u64,
        id: &str,
        resolution: QuestionResolution,
    ) -> Option<QuestionCompletion> {
        if epoch != self.epoch || self.disconnected {
            return None;
        }

        let draft = self
            .batches
            .iter_mut()
            .find(|draft| draft.id == id && draft.pending())?;

        let waiting = draft.mode != QuestionMode::Async;

        let (status, message, started_turn) = match resolution {
            QuestionResolution::Submitted {
                message,
                started_turn,
            } => (QuestionStatus::Submitted, message, started_turn),

            QuestionResolution::Skipped => (QuestionStatus::Skipped, None, false),
            QuestionResolution::Expired => (QuestionStatus::Expired, None, false),
        };

        draft.settle(status);

        Some(QuestionCompletion {
            message,
            started_turn,
            waiting_finished: waiting && !self.waiting(),
        })
    }

    pub fn submission_failed(&mut self, epoch: u64, id: &str, message: String) -> bool {
        if epoch != self.epoch || self.disconnected {
            return false;
        }

        let Some(draft) = self
            .batches
            .iter_mut()
            .find(|draft| draft.id == id && draft.status == QuestionStatus::Submitting)
        else {
            return false;
        };

        draft.status = QuestionStatus::Pending;
        draft.error = Some(QuestionError::Rejected(message));
        draft.touch();

        true
    }

    pub fn starting(&mut self, epoch: u64) {
        self.epoch = epoch;
        self.disconnect();
    }

    pub fn disconnect(&mut self) {
        self.disconnected = true;
        self.approval = None;

        for draft in &mut self.batches {
            if !draft.pending() {
                continue;
            }

            if draft.mode == QuestionMode::Async {
                draft.status = QuestionStatus::Pending;
                draft.error = Some(QuestionError::Disconnected);
            } else {
                draft.settle(QuestionStatus::Expired);
            }
        }
    }

    pub fn restore(&mut self, runtime: &mut SessionRuntime) {
        let Some(backend) = runtime.backend_mut() else {
            return;
        };

        let identity = backend.recovery_identity();
        let mut requests = Vec::new();

        for draft in &mut self.batches {
            if !draft.pending() {
                continue;
            }

            // Only an identified conversation can safely re-register an unanswered async request.
            if identity.is_none() || draft.identity != identity || draft.mode != QuestionMode::Async
            {
                draft.settle(QuestionStatus::Expired);

                continue;
            }

            if draft.status == QuestionStatus::Submitting {
                draft.status = QuestionStatus::Pending;
                draft.error = Some(QuestionError::Disconnected);
            }

            requests.push(QuestionRequest {
                id: draft.id.clone(),
                mode: draft.mode,
                questions: draft.questions.clone(),
            });
        }

        backend.restore_question_requests(requests);
        self.epoch = runtime.epoch();
        self.disconnected = false;
    }

    pub fn clear_questions(&mut self) {
        self.batches.clear();
    }
}
