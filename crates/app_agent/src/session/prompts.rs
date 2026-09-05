//! Approval display and independently addressable question drafts for one pane.

use nmt_agent_utils::chat::QuestionMode;

use crate::questions::{QuestionPrompt, QuestionStatus};

#[derive(Default)]
pub(crate) struct PendingPrompts {
    approval: Option<String>,
    pub(crate) batches: Vec<QuestionPrompt>,
    pub(crate) active: Option<usize>,
    pub(crate) collapsed: bool,
}

impl PendingPrompts {
    pub(crate) fn approval(&self) -> Option<&str> {
        self.approval.as_deref()
    }

    pub(crate) fn questions(&self) -> Option<&QuestionPrompt> {
        self.active.and_then(|index| self.batches.get(index))
    }

    pub(crate) fn questions_mut(&mut self) -> Option<&mut QuestionPrompt> {
        self.active.and_then(|index| self.batches.get_mut(index))
    }

    pub(crate) fn approval_open(&self) -> bool {
        self.approval.is_some()
    }

    pub(crate) fn questions_open(&self) -> bool {
        !self.collapsed && self.questions().is_some()
    }

    pub(crate) fn ask_approval(&mut self, description: String) {
        self.approval = Some(description);
    }

    pub(crate) fn ask_questions(&mut self, prompt: QuestionPrompt) {
        if let Some(index) = self.batches.iter().position(|entry| entry.id == prompt.id) {
            if prompt.id.is_none() || self.batches[index].status == QuestionStatus::History {
                self.batches[index] = prompt;
                self.active = Some(index);
                self.collapsed = false;
            }
            return;
        }
        let reveal = self.questions().is_none_or(|question| {
            !question.pending()
                || (prompt.mode != QuestionMode::Async && question.mode == QuestionMode::Async)
        });
        self.batches.push(prompt);
        if reveal {
            self.active = Some(self.batches.len() - 1);
            self.collapsed = false;
        }
    }

    /// A settled batch has nothing left to answer, so the panel steps to the
    /// next batch that still does and otherwise hands the space back to the
    /// composer. A history card stays because the user opened it to read.
    pub(crate) fn hide_settled(&mut self) {
        let settled = self
            .questions()
            .is_some_and(|prompt| !prompt.pending() && prompt.status != QuestionStatus::History);
        if settled {
            self.active = self.batches.iter().position(QuestionPrompt::pending);
        }
    }

    pub(crate) fn dismiss_approval(&mut self) {
        self.approval = None;
    }

    pub(crate) fn dismiss_questions(&mut self) {
        self.batches.clear();
        self.active = None;
        self.collapsed = false;
    }

    pub(crate) fn pending_count(&self) -> usize {
        self.batches
            .iter()
            .filter(|prompt| prompt.pending())
            .count()
    }
}
