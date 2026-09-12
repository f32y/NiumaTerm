//! Active panel selection and widgets for core-owned input requests.

use nmt_agent::chat::{Question, QuestionInput, QuestionMode};
use nmt_agent::session::input::{QuestionDraft, QuestionStatus, SessionInput};

use crate::agent_tab::questions::QuestionPresentation;

#[derive(Default)]
pub(in crate::agent_tab) struct PendingPrompts {
    pub(in crate::agent_tab) presentations: Vec<QuestionPresentation>,
    pub(in crate::agent_tab) active: Option<usize>,
    pub(in crate::agent_tab) collapsed: bool,
}

impl PendingPrompts {
    pub(in crate::agent_tab) fn questions<'a>(
        &self,
        input: &'a SessionInput,
    ) -> Option<&'a QuestionDraft> {
        self.active.and_then(|index| input.batches().get(index))
    }

    pub(in crate::agent_tab) fn questions_mut<'a>(
        &mut self,
        input: &'a mut SessionInput,
    ) -> Option<&'a mut QuestionDraft> {
        let key = self.questions(input)?.key();

        input.draft_mut(key)
    }

    pub(in crate::agent_tab) fn questions_open(&self, input: &SessionInput) -> bool {
        !self.collapsed && self.questions(input).is_some()
    }

    pub(in crate::agent_tab) fn reveal(&mut self, input: &SessionInput, index: usize) {
        let prompt = &input.batches()[index];

        let reveal = index < self.presentations.len()
            || self.questions(input).is_none_or(|question| {
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

    pub(in crate::agent_tab) fn open_history(
        &mut self,
        input: &mut SessionInput,
        item_id: &str,
        questions: Vec<Question>,
    ) {
        let index = input.history(item_id, questions);

        if index == self.presentations.len() {
            self.presentations
                .push(QuestionPresentation::new(&input.batches()[index]));
        }

        self.active = Some(index);
        self.collapsed = false;
    }

    /// Completed batches release the panel; explicitly opened history stays visible.
    pub(in crate::agent_tab) fn hide_settled(&mut self, input: &SessionInput) {
        let settled = self
            .questions(input)
            .is_some_and(|prompt| !prompt.pending() && prompt.status() != QuestionStatus::History);

        if settled {
            self.active = input.batches().iter().position(QuestionDraft::pending);
        }

        self.release_secret_editors(input);
    }

    pub(in crate::agent_tab) fn release_secret_editors(&mut self, input: &SessionInput) {
        for (draft, presentation) in input.batches().iter().zip(&mut self.presentations) {
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

    pub(in crate::agent_tab) fn reset_editors(&mut self) {
        for presentation in &mut self.presentations {
            for editor in &mut presentation.editors {
                *editor = None;
            }
        }
    }

    pub(in crate::agent_tab) fn clear(&mut self) {
        self.presentations.clear();
        self.active = None;
        self.collapsed = false;
    }
}
