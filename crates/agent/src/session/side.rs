#[cfg(test)]
#[path = "side_tests.rs"]
mod tests;

use std::cell::RefCell;
use std::rc::Rc;

use crate::chat::Item;
use crate::transcript::conversation::ConversationState;

/// What asking a side question did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SideQuestionOutcome {
    Asked,
    /// An earlier question is still unanswered. Follow-ups carry the earlier
    /// answers as their history, so a question asked before those arrive
    /// would be answered without them.
    Busy,
    /// The harness has no way to answer outside the conversation.
    Unsupported,
    Failed(String),
}

/// The question waiting for an answer, and the turn it opened.
struct PendingSide {
    request_id: String,
    question: String,
    turn: u64,
}

/// The questions asked beside the conversation and their answers.
///
/// The exchanges are kept as a conversation of their own, one turn per
/// question, so any transcript view can render them exactly as it renders the
/// main conversation. Kept only in memory: side questions are never part of
/// the conversation, so there is nothing to restore them into.
#[derive(Default)]
pub struct SideQuestions {
    conversation: Rc<RefCell<ConversationState>>,

    /// The answered exchanges, oldest first, which a follow-up sends back as
    /// its history because the harness keeps none of its own.
    answered: Vec<(String, String)>,

    pending: Option<PendingSide>,
    turns: u64,
}

impl SideQuestions {
    /// The exchanges as a conversation, shared with the view rendering them.
    pub fn conversation(&self) -> &Rc<RefCell<ConversationState>> {
        &self.conversation
    }

    pub fn is_open(&self) -> bool {
        self.turns > 0
    }

    pub(crate) fn pending(&self) -> Option<&str> {
        self.pending
            .as_ref()
            .map(|pending| pending.request_id.as_str())
    }

    /// The answered exchanges a follow-up can refer back to. A failed one is
    /// left out because the backend never produced what it would claim to
    /// have said.
    pub(crate) fn history(&self) -> Vec<(&str, &str)> {
        self.answered
            .iter()
            .map(|(question, answer)| (question.as_str(), answer.as_str()))
            .collect()
    }

    pub(crate) fn ask(&mut self, question: String, request_id: String) {
        self.turns += 1;

        let mut conversation = self.conversation.borrow_mut();

        conversation.push(
            self.turns,
            Item::UserMessage {
                text: Some(question.clone()),
            },
            Vec::new(),
        );

        conversation.start();

        self.pending = Some(PendingSide {
            request_id,
            question,
            turn: self.turns,
        });
    }

    /// Record the answer to request `id`. Returns whether a question was
    /// waiting on it: an answer arriving after the questions were closed
    /// belongs to nothing still shown.
    pub(crate) fn settle(&mut self, id: &str, answer: Result<String, String>) -> bool {
        let Some(pending) = self.pending.take_if(|pending| pending.request_id == id) else {
            return false;
        };

        let item = match answer {
            Ok(text) => {
                self.answered.push((pending.question, text.clone()));

                Item::AgentMessage {
                    id: pending.request_id,
                    text: Some(text),
                    questions: None,
                }
            }
            Err(text) => Item::Error { text },
        };

        let mut conversation = self.conversation.borrow_mut();

        conversation.push(pending.turn, item, Vec::new());

        conversation.settle(pending.turn);

        true
    }

    /// Drop every exchange, returning the request still waiting for an answer
    /// so the caller can stop it. The conversation is emptied in place because
    /// a view may still hold it.
    pub(crate) fn close(&mut self) -> Option<String> {
        self.conversation.borrow_mut().clear();

        self.answered.clear();

        self.turns = 0;

        self.pending.take().map(|pending| pending.request_id)
    }
}
