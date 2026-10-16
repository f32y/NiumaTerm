//! Answer drafts and completion rules, independent of input widgets.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::chat::{Question, QuestionAnswer, QuestionInput, QuestionMode, QuestionRequest};
use crate::session::RecoveryIdentity;
use crate::session::input::{QuestionError, QuestionKey};
use crate::session::view::{DraftAnswers, DraftView, Since};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

    /// The answers typed so far, for a view that sends them elsewhere.
    pub fn draft_answers(&self) -> DraftAnswers {
        DraftAnswers {
            selected: self.selected.clone(),
            text: self.text.clone(),
            custom: self.custom.clone(),
        }
    }

    /// Take answers composed in another view. A batch that already left
    /// the pending state keeps the answers it was settled with.
    pub fn set_answers(&mut self, answers: DraftAnswers) {
        let count = self.questions.len();

        if self.status != QuestionStatus::Pending
            || answers.selected.len() != count
            || answers.text.len() != count
            || answers.custom.len() != count
        {
            return;
        }

        self.selected = answers.selected;
        self.text = answers.text;
        self.custom = answers.custom;

        self.touch();
    }

    pub fn view(&self) -> DraftView {
        DraftView {
            id: self.id.clone(),
            questions: self.questions.clone(),
            mode: self.mode,
            status: self.status,
            error: self.error.clone(),
            selected: self.selected.clone(),
            text: self.text.clone(),
            custom: self.custom.clone(),
            key: self.key,
            started: Since::of(Some(self.started)),
            touched: self.touched,
        }
    }

    /// A draft mirrored from another process. Its conversation identity
    /// stays there, with the only code that restores questions.
    pub fn from_view(view: DraftView) -> Self {
        Self {
            id: view.id,
            identity: None,
            questions: view.questions,
            mode: view.mode,
            status: view.status,
            error: view.error,
            selected: view.selected,
            text: view.text,
            custom: view.custom,
            key: view.key,
            started: view.started.instant().unwrap_or_else(Instant::now),
            touched: view.touched,
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

    /// Whether question `question` has an answer: typed text where text was
    /// chosen or is the only way to answer, a pick otherwise.
    pub fn is_answered(&self, question: usize) -> bool {
        let Some(entry) = self.questions.get(question) else {
            return false;
        };

        if self.custom[question] || entry.options.is_empty() {
            !self.text[question].trim().is_empty()
        } else {
            !self.selected[question].is_empty()
        }
    }

    pub fn is_complete(&self) -> bool {
        !self.questions.is_empty() && (0..self.questions.len()).all(|index| self.is_answered(index))
    }

    pub fn answers(&self) -> Vec<QuestionAnswer> {
        self.questions
            .iter()
            .zip(&self.selected)
            .enumerate()
            .map(|(index, (question, picks))| {
                let labels = || {
                    picks
                        .iter()
                        .filter_map(|index| question.options.get(*index))
                        .map(|option| option.label.clone())
                        .collect::<Vec<_>>()
                };

                // A typed answer is the one the user chose to give, so it
                // stands alone: a pick the card displays as unselected is not
                // part of what they answered with.
                if self.custom[index] || question.options.is_empty() {
                    QuestionAnswer {
                        selected: Vec::new(),
                        custom: Some(self.text[index].trim().to_string()),
                    }
                } else {
                    QuestionAnswer {
                        selected: labels(),
                        custom: None,
                    }
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
