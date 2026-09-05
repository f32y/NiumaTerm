//! Question drafts are independent of the composer and of the running turn.

mod actions;
mod render;

use std::time::{Duration, Instant};

use gpui::{Entity, Subscription};
use gpui_component::input::InputState;
use nmt_agent_utils::chat::{Question, QuestionInput, QuestionMode, QuestionRequest};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum QuestionStatus {
    Pending,
    Submitting,
    Submitted,
    Skipped,
    Expired,
    History,
}

pub(super) struct QuestionEditor {
    state: Entity<InputState>,
    _subscription: Subscription,
}

pub(crate) struct QuestionPrompt {
    pub(crate) id: Option<String>,
    pub(crate) thread_id: Option<String>,
    pub(crate) questions: Vec<Question>,
    pub(crate) mode: QuestionMode,
    pub(crate) status: QuestionStatus,
    pub(crate) error: Option<String>,
    selected: Vec<Vec<usize>>,
    text: Vec<String>,
    custom: Vec<bool>,
    editors: Vec<Option<QuestionEditor>>,
    pub(crate) focus: (usize, usize),
    started: Instant,
    touched: bool,
}

impl QuestionPrompt {
    pub(crate) fn new(questions: Vec<Question>) -> Self {
        let count = questions.len();
        Self {
            id: None,
            thread_id: None,
            questions,
            mode: QuestionMode::Blocking,
            status: QuestionStatus::Pending,
            error: None,
            selected: vec![Vec::new(); count],
            text: vec![String::new(); count],
            custom: vec![false; count],
            editors: (0..count).map(|_| None).collect(),
            focus: (0, 0),
            started: Instant::now(),
            touched: false,
        }
    }

    pub(crate) fn from_request(request: QuestionRequest) -> Self {
        let mut prompt = Self::new(request.questions);
        prompt.id = Some(request.id);
        prompt.mode = request.mode;
        if request.mode == QuestionMode::Async {
            for (question, selected) in prompt.questions.iter().zip(&mut prompt.selected) {
                if !question.options.is_empty() {
                    selected.push(0);
                }
            }
        }
        prompt
    }

    pub(crate) fn pending(&self) -> bool {
        matches!(
            self.status,
            QuestionStatus::Pending | QuestionStatus::Submitting
        )
    }

    pub(crate) fn touch(&mut self) {
        self.touched = true;
    }

    pub(crate) fn auto_resolve_remaining(&self) -> Option<Duration> {
        (self.mode == QuestionMode::Optional
            && !self.touched
            && self.status == QuestionStatus::Pending)
            .then(|| Duration::from_secs(120).saturating_sub(self.started.elapsed()))
    }

    pub(crate) fn is_selected(&self, question: usize, option: usize) -> bool {
        !self.custom[question] && self.selected[question].contains(&option)
    }

    pub(crate) fn is_focused(&self, question: usize, option: usize) -> bool {
        self.focus == (question, option)
    }

    pub(crate) fn move_focus(&mut self, forward: bool) -> bool {
        if self.status != QuestionStatus::Pending {
            return false;
        }
        let order: Vec<(usize, usize)> = self
            .questions
            .iter()
            .enumerate()
            .flat_map(|(question, entry)| {
                (0..entry.options.len()).map(move |option| (question, option))
            })
            .collect();
        if order.is_empty() {
            return false;
        }
        self.touch();
        let Some(current) = order.iter().position(|entry| *entry == self.focus) else {
            self.focus = order[0];
            return true;
        };
        let next = if forward {
            (current + 1) % order.len()
        } else {
            (current + order.len() - 1) % order.len()
        };
        self.focus = order[next];
        true
    }

    pub(crate) fn toggle(&mut self, question: usize, option: usize) {
        let Some(entry) = self.questions.get(question) else {
            return;
        };
        if self.status != QuestionStatus::Pending || option >= entry.options.len() {
            return;
        }
        let multi_select = entry.multi_select;
        self.touch();
        self.custom[question] = false;
        self.focus = (question, option);
        let picks = &mut self.selected[question];
        if !multi_select {
            *picks = vec![option];
        } else {
            match picks.iter().position(|picked| *picked == option) {
                Some(index) => {
                    picks.remove(index);
                }
                None => {
                    picks.push(option);
                    picks.sort_unstable();
                }
            }
        }
    }

    pub(crate) fn is_complete(&self) -> bool {
        !self.questions.is_empty()
            && self.selected.iter().enumerate().all(|(index, picks)| {
                if self.custom[index] || self.questions[index].options.is_empty() {
                    !self.text[index].trim().is_empty()
                } else {
                    !picks.is_empty()
                }
            })
    }

    pub(crate) fn answers(&self) -> Vec<Vec<String>> {
        self.questions
            .iter()
            .zip(&self.selected)
            .enumerate()
            .map(|(index, (question, picks))| {
                if self.custom[index] || question.options.is_empty() {
                    vec![self.text[index].trim().to_string()]
                } else {
                    picks
                        .iter()
                        .filter_map(|index| question.options.get(*index))
                        .map(|option| option.label.clone())
                        .collect()
                }
            })
            .collect()
    }

    pub(crate) fn reset_editors(&mut self) {
        self.editors = (0..self.questions.len()).map(|_| None).collect();
    }

    pub(crate) fn settle(&mut self, status: QuestionStatus) {
        self.status = status;
        self.error = None;
        // Secret values are used only for the live response, never for a history card.
        for (index, question) in self.questions.iter().enumerate() {
            if question.input == QuestionInput::Secret {
                self.text[index].clear();
                self.editors[index] = None;
            }
        }
    }
}

#[cfg(test)]
mod tests;
