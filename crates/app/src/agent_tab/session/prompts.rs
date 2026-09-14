//! Active panel selection and widgets for core-owned input requests.

use std::collections::HashMap;

use nmt_agent::chat::{Question, QuestionInput, QuestionMode};
use nmt_agent::session::input::{QuestionDraft, QuestionKey, QuestionStatus, SessionInput};

use crate::agent_tab::questions::QuestionPresentation;

#[derive(Default)]
pub(crate) struct PendingPrompts {
    pub(crate) presentations: HashMap<QuestionKey, QuestionPresentation>,
    pub(crate) active: Option<QuestionKey>,
    pub(crate) collapsed: bool,
}

impl PendingPrompts {
    pub(crate) fn questions<'a>(&self, input: &'a SessionInput) -> Option<&'a QuestionDraft> {
        self.active.and_then(|key| input.draft(key))
    }

    pub(crate) fn questions_mut<'a>(
        &mut self,
        input: &'a mut SessionInput,
    ) -> Option<&'a mut QuestionDraft> {
        input.draft_mut(self.active?)
    }

    pub(crate) fn questions_open(&self, input: &SessionInput) -> bool {
        !self.collapsed && self.questions(input).is_some()
    }

    pub(crate) fn reveal(&mut self, input: &SessionInput, index: usize) {
        let Some(prompt) = input.batches().get(index) else {
            return;
        };

        let key = prompt.key();

        let reveal = self.presentations.contains_key(&key)
            || self.questions(input).is_none_or(|question| {
                !question.pending()
                    || (prompt.mode() != QuestionMode::Async
                        && question.mode() == QuestionMode::Async)
            });

        self.presentations
            .retain(|key, _| input.draft(*key).is_some());

        self.presentations
            .entry(key)
            .or_insert_with(|| QuestionPresentation::new(prompt));

        if reveal {
            self.active = Some(key);
            self.collapsed = false;
        }
    }

    pub(crate) fn open_history(
        &mut self,
        input: &mut SessionInput,
        item_id: &str,
        questions: Vec<Question>,
    ) {
        let index = input.history(item_id, questions);

        let prompt = &input.batches()[index];
        let key = prompt.key();

        self.presentations
            .entry(key)
            .or_insert_with(|| QuestionPresentation::new(prompt));

        self.active = Some(key);
        self.collapsed = false;
    }

    /// Completed batches release the panel; explicitly opened history stays visible.
    pub(crate) fn hide_settled(&mut self, input: &SessionInput) {
        let settled = self
            .questions(input)
            .is_none_or(|prompt| !prompt.pending() && prompt.status() != QuestionStatus::History);

        if settled {
            self.active = input
                .batches()
                .iter()
                .find(|draft| draft.pending())
                .map(|draft| draft.key());
        }

        self.release_secret_editors(input);
    }

    pub(crate) fn release_secret_editors(&mut self, input: &SessionInput) {
        self.presentations.retain(|key, presentation| {
            let Some(draft) = input.draft(*key) else {
                return false;
            };

            if !draft.pending() {
                for (question, editor) in draft.questions().iter().zip(&mut presentation.editors) {
                    if question.input == QuestionInput::Secret {
                        *editor = None;
                    }
                }
            }

            true
        });
    }

    pub(crate) fn reset_editors(&mut self) {
        for presentation in self.presentations.values_mut() {
            for editor in &mut presentation.editors {
                *editor = None;
            }
        }
    }

    pub(crate) fn clear(&mut self) {
        self.presentations.clear();

        self.active = None;
        self.collapsed = false;
    }
}
