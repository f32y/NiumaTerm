#[cfg(test)]
#[path = "side_tests.rs"]
mod tests;

/// Where one side question's answer stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SideAnswer {
    /// Waiting on the backend request named here.
    Pending(String),
    Answered(String),
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SideExchange {
    pub question: String,
    pub answer: SideAnswer,
}

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

/// The questions asked beside the conversation and their answers, oldest
/// first. Kept only in memory: side questions are never part of the
/// conversation, so there is nothing to restore them into.
#[derive(Default)]
pub struct SideQuestions {
    exchanges: Vec<SideExchange>,
}

impl SideQuestions {
    pub fn exchanges(&self) -> &[SideExchange] {
        &self.exchanges
    }

    pub fn is_open(&self) -> bool {
        !self.exchanges.is_empty()
    }

    pub(crate) fn pending(&self) -> Option<&str> {
        self.exchanges
            .iter()
            .find_map(|exchange| match &exchange.answer {
                SideAnswer::Pending(id) => Some(id.as_str()),
                SideAnswer::Answered(_) | SideAnswer::Failed(_) => None,
            })
    }

    /// The answered exchanges a follow-up can refer back to. A failed one is
    /// left out because the backend never produced what it would claim to
    /// have said.
    pub(crate) fn history(&self) -> Vec<(&str, &str)> {
        self.exchanges
            .iter()
            .filter_map(|exchange| match &exchange.answer {
                SideAnswer::Answered(answer) => Some((exchange.question.as_str(), answer.as_str())),
                SideAnswer::Pending(_) | SideAnswer::Failed(_) => None,
            })
            .collect()
    }

    pub(crate) fn ask(&mut self, question: String, request_id: String) {
        self.exchanges.push(SideExchange {
            question,
            answer: SideAnswer::Pending(request_id),
        });
    }

    /// Record the answer to request `id`. Returns whether a question was
    /// waiting on it: an answer arriving after the questions were closed
    /// belongs to nothing still shown.
    pub(crate) fn settle(&mut self, id: &str, answer: Result<String, String>) -> bool {
        let Some(exchange) = self.exchanges.iter_mut().find(
            |exchange| matches!(&exchange.answer, SideAnswer::Pending(pending) if pending == id),
        ) else {
            return false;
        };

        exchange.answer = match answer {
            Ok(text) => SideAnswer::Answered(text),
            Err(message) => SideAnswer::Failed(message),
        };

        true
    }

    /// Drop every exchange, returning the request still waiting for an answer
    /// so the caller can stop it.
    pub(crate) fn close(&mut self) -> Option<String> {
        let pending = self.pending().map(str::to_owned);

        self.exchanges.clear();

        pending
    }
}
