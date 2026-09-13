//! Answer drafts and completion rules, independent of input widgets.

use std::time::{Duration, Instant};

use crate::chat::{Question, QuestionInput, QuestionMode, QuestionRequest};
use crate::session::RecoveryIdentity;
use crate::session::input::{QuestionError, QuestionKey};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuestionStatus {
    Pending,
    Submitting,
    Submitted,
    Skipped,
    Expired,
    History,
}

pub struct QuestionDraft {
    pub(super) id: String,
    pub(super) identity: Option<RecoveryIdentity>,
    pub(super) questions: Vec<Question>,
    pub(super) mode: QuestionMode,
    pub(super) status: QuestionStatus,
    pub(super) error: Option<QuestionError>,
    selected: Vec<Vec<usize>>,
    text: Vec<String>,
    custom: Vec<bool>,
    pub(super) key: QuestionKey,
    started: Instant,
    touched: bool,
}

impl QuestionDraft {
    pub fn key(&self) -> QuestionKey {
        self.key
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn questions(&self) -> &[Question] {
        &self.questions
    }

    pub fn mode(&self) -> QuestionMode {
        self.mode
    }

    pub fn status(&self) -> QuestionStatus {
        self.status
    }

    pub fn error(&self) -> Option<&QuestionError> {
        self.error.as_ref()
    }

    pub fn text(&self, question: usize) -> &str {
        self.text.get(question).map_or("", String::as_str)
    }

    pub fn is_custom(&self, question: usize) -> bool {
        self.custom.get(question).copied().unwrap_or(false)
    }

    pub fn choose_custom(&mut self, question: usize) -> bool {
        if self.status != QuestionStatus::Pending
            || self
                .questions
                .get(question)
                .is_none_or(|q| q.input == QuestionInput::SelectionOnly)
        {
            return false;
        }

        self.custom[question] = true;
        self.touch();

        true
    }

    pub fn set_text(&mut self, question: usize, value: String) -> bool {
        if self.text.get(question) == Some(&value) || !self.choose_custom(question) {
            return false;
        }

        self.text[question] = value;

        true
    }

    pub fn new(id: String, questions: Vec<Question>) -> Self {
        let count = questions.len();

        Self {
            id,
            identity: None,
            questions,
            mode: QuestionMode::Blocking,
            status: QuestionStatus::Pending,
            error: None,
            selected: vec![Vec::new(); count],
            text: vec![String::new(); count],
            custom: vec![false; count],
            key: QuestionKey {
                index: 0,
                generation: 0,
            },
            started: Instant::now(),
            touched: false,
        }
    }

    pub fn pending(&self) -> bool {
        matches!(
            self.status,
            QuestionStatus::Pending | QuestionStatus::Submitting
        )
    }

    pub fn touch(&mut self) {
        self.touched = true;
    }

    pub fn auto_resolve_remaining(&self, now: Instant) -> Option<Duration> {
        (self.mode == QuestionMode::Optional
            && !self.touched
            && self.status == QuestionStatus::Pending)
            .then(|| {
                Duration::from_secs(120).saturating_sub(now.saturating_duration_since(self.started))
            })
    }

    pub fn is_selected(&self, question: usize, option: usize) -> bool {
        !self.is_custom(question)
            && self
                .selected
                .get(question)
                .is_some_and(|picks| picks.contains(&option))
    }

    pub fn toggle(&mut self, question: usize, option: usize) {
        let Some(entry) = self.questions.get(question) else {
            return;
        };

        if self.status != QuestionStatus::Pending || option >= entry.options.len() {
            return;
        }

        let multi_select = entry.multi_select;

        self.touch();
        self.custom[question] = false;

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

    pub fn is_complete(&self) -> bool {
        !self.questions.is_empty()
            && self.selected.iter().enumerate().all(|(index, picks)| {
                if self.custom[index] || self.questions[index].options.is_empty() {
                    !self.text[index].trim().is_empty()
                } else {
                    !picks.is_empty()
                }
            })
    }

    pub fn answers(&self) -> Vec<Vec<String>> {
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

    pub(super) fn settle(&mut self, status: QuestionStatus) {
        self.status = status;
        self.error = None;

        // Secret values are used only for the live response, never for a history card.
        for (index, question) in self.questions.iter().enumerate() {
            if question.input == QuestionInput::Secret {
                self.text[index].clear();
            }
        }
    }
}

impl From<QuestionRequest> for QuestionDraft {
    fn from(request: QuestionRequest) -> Self {
        let mut prompt = Self::new(request.id, request.questions);

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
}
