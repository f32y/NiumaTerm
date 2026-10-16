//! Questions retain their own identity because several batches can outlive a turn.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum QuestionInput {
    #[default]
    SelectionOnly,
    Text,
    Secret,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionOption {
    pub label: String,
    pub description: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Question {
    pub header: Option<String>,
    pub question: String,
    pub multi_select: bool,
    pub options: Vec<QuestionOption>,
    pub input: QuestionInput,
}

/// One question's answer as the backend receives it.
///
/// A question can be answered by picking an option or by typing something the
/// options do not cover, and the two travel as separate fields because the
/// protocols that carry free text ask for them separately: the typed answer
/// replaces the pick on a single-select question and supplements it on a
/// multi-select one. An answer that picked nothing and typed nothing is a skip,
/// which the caller expresses by sending no answers at all.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionAnswer {
    /// Labels the user picked, in option order.
    #[serde(default)]
    pub selected: Vec<String>,

    /// What the user typed instead of picking. Absent when they picked.
    #[serde(default)]
    pub custom: Option<String>,
}

impl QuestionAnswer {
    /// The answer as a backend whose protocol takes one string per question
    /// reads it: the labels, followed by the typed answer when there is one.
    ///
    /// This is the shape every backend received before typed answers had a
    /// field of their own, so a backend that cannot name a typed answer keeps
    /// reporting exactly what it reported then. A typed answer reaches those
    /// backends as the sole value of its question.
    pub fn values(&self) -> Vec<String> {
        let mut values = self.selected.clone();

        if let Some(custom) = &self.custom {
            values.push(custom.clone());
        }

        values
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum QuestionMode {
    Blocking,
    /// The request can be skipped after a visible countdown until the user interacts.
    Optional,
    /// Answers arrive as new user messages and remain valid after the asking turn ends.
    Async,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuestionRequest {
    pub id: String,
    pub mode: QuestionMode,
    pub questions: Vec<Question>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QuestionResolution {
    Submitted {
        message: Option<String>,
        started_turn: bool,
    },
    Skipped,
    Expired,
}

/// Whether the provider will report completion in a later event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuestionResponse {
    Settled,
    Pending,
}
