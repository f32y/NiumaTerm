//! Active panel selection and widgets for core-owned input requests.

use nmt_agent::chat::{Question, QuestionInput, QuestionMode, QuestionRequest};
use nmt_agent::session::SessionRuntime;
use nmt_agent::session::input::{QuestionDraft, QuestionStatus, SessionInput};

use crate::questions::QuestionPresentation;

#[derive(Default)]
pub(crate) struct PendingPrompts {
    pub(crate) core: SessionInput,
    pub(crate) presentations: Vec<QuestionPresentation>,
    pub(crate) active: Option<usize>,
    pub(crate) collapsed: bool,
}

impl PendingPrompts {
    pub(crate) fn approval(&self) -> Option<&str> {
        self.core.approval()
    }

    pub(crate) fn approval_open(&self) -> bool {
        self.approval().is_some()
    }

    pub(crate) fn dismiss_approval(&mut self) {
        self.core.dismiss_approval();
    }

    pub(crate) fn pending_count(&self) -> usize {
        self.core.pending_count()
    }

    pub(crate) fn questions(&self) -> Option<&QuestionDraft> {
        self.active.and_then(|index| self.core.batches().get(index))
    }

    pub(crate) fn questions_mut(&mut self) -> Option<&mut QuestionDraft> {
        let key = self.questions()?.key();
        self.core.draft_mut(key)
    }

    pub(crate) fn questions_open(&self) -> bool {
        !self.collapsed && self.questions().is_some()
    }

    pub(crate) fn receive(
        &mut self,
        runtime: &SessionRuntime,
        request: QuestionRequest,
    ) -> Option<usize> {
        let index = self.core.receive(runtime, request)?;
        self.reveal(index);
        Some(index)
    }

    pub(crate) fn ask_questions(&mut self, questions: Vec<Question>) {
        let index = self.core.receive_legacy(questions);
        self.reveal(index);
    }

    fn reveal(&mut self, index: usize) {
        let prompt = &self.core.batches()[index];
        let reveal = index < self.presentations.len()
            || self.questions().is_none_or(|question| {
                !question.pending()
                    || (prompt.mode() != QuestionMode::Async
                        && question.mode() == QuestionMode::Async)
            });
        let presentation = QuestionPresentation::new(prompt);
        if index < self.presentations.len() {
            self.presentations[index] = presentation;
        } else {
            self.presentations.push(presentation);
        }
        if reveal {
            self.active = Some(index);
            self.collapsed = false;
        }
    }

    pub(crate) fn open_history(&mut self, item_id: &str, questions: Vec<Question>) {
        let index = self.core.history(item_id, questions);
        if index == self.presentations.len() {
            self.presentations
                .push(QuestionPresentation::new(&self.core.batches()[index]));
        }
        self.active = Some(index);
        self.collapsed = false;
    }

    /// Completed batches release the panel; explicitly opened history stays visible.
    pub(crate) fn hide_settled(&mut self) {
        let settled = self
            .questions()
            .is_some_and(|prompt| !prompt.pending() && prompt.status() != QuestionStatus::History);
        if settled {
            self.active = self.core.batches().iter().position(QuestionDraft::pending);
        }
        self.release_secret_editors();
    }

    pub(crate) fn release_secret_editors(&mut self) {
        for (draft, presentation) in self.core.batches().iter().zip(&mut self.presentations) {
            if draft.pending() {
                continue;
            }
            for (question, editor) in draft.questions().iter().zip(&mut presentation.editors) {
                if question.input == QuestionInput::Secret {
                    *editor = None;
                }
            }
        }
    }

    pub(crate) fn reset_editors(&mut self) {
        for presentation in &mut self.presentations {
            for editor in &mut presentation.editors {
                *editor = None;
            }
        }
    }

    pub(crate) fn dismiss_questions(&mut self) {
        self.core.clear_questions();
        self.presentations.clear();
        self.active = None;
        self.collapsed = false;
    }
}
