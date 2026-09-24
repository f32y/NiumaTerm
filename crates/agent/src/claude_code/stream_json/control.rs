#[cfg(test)]
#[path = "control_tests.rs"]
mod control_tests;

use std::collections::{HashMap, VecDeque};
use std::time::Instant;

use serde_json::{Value, json};
use tracing::debug;

use crate::chat::{
    ContextComposition, ContextSegment, Event, Question, QuestionOption, QuestionResolution,
};
use crate::subprocess::InputTicket;
use crate::subprocess::requests::{DeadlineTimer, RequestClass};

/// One request the CLI has not answered yet: when it expires, the queued
/// write it may still be waiting in, and what its answer completes.
struct PendingControl {
    at: Instant,
    class: RequestClass,
    input: Option<InputTicket>,
    operation: PendingControlOperation,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum PendingControlOperation {
    /// The initialize handshake; losing it ends the session.
    Init,
    /// An effort change. Its outcome is settled in submission order by
    /// `EffortState`, so the table only keeps its deadline.
    Effort,
    Other,
    FileRewind,
    ContextComposition,
    SessionTitle,
    /// A side question, named by its request id so the answer can be matched
    /// to the question that asked it.
    SideQuestion(String),
}

/// A request that ran out of time, with what is needed to settle it.
pub(super) struct ExpiredControl {
    pub(super) id: String,
    pub(super) class: RequestClass,
    pub(super) input: Option<InputTicket>,
    pub(super) operation: PendingControlOperation,
}

pub(super) struct ControlState {
    timer: Option<DeadlineTimer>,
    next_id: u64,
    closed: bool,
    pending: HashMap<String, PendingControl>,
    effort: EffortState,
    pub(super) pending_approval: Option<PendingApproval>,
    pub(super) pending_questions: Option<PendingQuestions>,
}

impl Default for ControlState {
    fn default() -> Self {
        Self {
            timer: None,
            next_id: 1,
            closed: false,
            pending: HashMap::new(),
            effort: EffortState::default(),
            pending_approval: None,
            pending_questions: None,
        }
    }
}

impl ControlState {
    pub(super) fn set_timer(&mut self, timer: DeadlineTimer) {
        self.timer = Some(timer);

        self.refresh_timer();
    }

    fn refresh_timer(&self) {
        if let Some(timer) = &self.timer {
            timer
                .handle()
                .set(self.pending.values().map(|pending| pending.at).min());
        }
    }

    pub(super) fn check_connected(&self) -> Result<(), String> {
        if self.closed {
            return Err("Claude is not connected".into());
        }

        Ok(())
    }

    /// Record a request the writer accepted. The deadline starts at
    /// admission: a request that times out can still be waiting in the
    /// writer, which is why the queued write is kept beside it.
    pub(super) fn admit(
        &mut self,
        id: String,
        class: RequestClass,
        input: Option<InputTicket>,
        operation: PendingControlOperation,
        now: Instant,
    ) {
        if self.closed {
            return;
        }

        self.pending.insert(
            id,
            PendingControl {
                at: now + class.timeout(),
                class,
                input,
                operation,
            },
        );

        self.refresh_timer();
    }

    pub(super) fn complete(&mut self, id: &str) {
        self.pending.remove(id);

        self.refresh_timer();
    }

    pub(super) fn expired(&mut self, now: Instant) -> Vec<ExpiredControl> {
        let expired = self
            .pending
            .extract_if(|_, pending| pending.at <= now)
            .map(|(id, pending)| ExpiredControl {
                id,
                class: pending.class,
                input: pending.input,
                operation: pending.operation,
            })
            .collect();

        self.refresh_timer();

        expired
    }

    pub(super) fn expire_effort(&mut self, id: &str) -> Option<Vec<Event>> {
        self.effort
            .contains(id)
            .then(|| self.effort.expire(id).into_iter().collect())
    }

    /// Settle an expired request whose answer can no longer be applied: a
    /// rejected effort change, a failed file restore, or otherwise one
    /// non-fatal error naming the timeout.
    pub(super) fn fail_expired(&mut self, expired: ExpiredControl, message: &str) -> Vec<Event> {
        let mut events: Vec<Event> = if self.effort.contains(&expired.id) {
            self.effort
                .resolve(&expired.id, Some(message.to_string()))
                .into_iter()
                .collect()
        } else {
            fail_pending_control_operations([expired.operation], message)
        };

        if events.is_empty() {
            events.push(Event::Error {
                message: message.to_string(),
                fatal: false,
            });
        }

        events
    }

    pub(super) fn with_effort(value: Option<String>) -> Self {
        Self {
            effort: EffortState::new(value),
            ..Self::default()
        }
    }

    pub(super) fn effort(&self) -> Option<&str> {
        self.effort.desired()
    }

    pub(super) fn record_effort(&mut self, id: String, value: String) {
        if self.closed {
            return;
        }

        if let Some(pending) = self.pending.get_mut(&id) {
            pending.operation = PendingControlOperation::Effort;
        }

        self.effort.record(id, value);
    }

    pub(super) fn request(&mut self, request: Value) -> (String, Value) {
        let request_id = format!("nmt-{}", self.next_id);

        self.next_id += 1;

        let message = json!({
            "type": "control_request", "request_id": request_id, "request": request,
        });

        (request_id, message)
    }

    pub(super) fn contains(&self, operation: &PendingControlOperation) -> bool {
        self.pending
            .values()
            .any(|pending| pending.operation == *operation)
    }

    pub(super) fn has_active_request(&self) -> bool {
        self.effort.has_pending()
            || self.pending_approval.is_some()
            || self.pending_questions.is_some()
            || self.pending.values().any(|pending| {
                matches!(
                    pending.operation,
                    PendingControlOperation::FileRewind
                        | PendingControlOperation::ContextComposition
                        | PendingControlOperation::SessionTitle
                )
            })
    }

    pub(super) fn resolve(&mut self, response: &Value) -> Option<Event> {
        let id = response["request_id"].as_str()?;

        let pending = self.pending.remove(id);

        self.refresh_timer();

        if self.effort.contains(id) {
            return self.effort.resolve(id, control_response_error(response));
        }

        operation_resolved(pending?.operation, response)
    }

    pub(super) fn cancel_generated_title(&mut self) {
        self.pending.retain(|_, pending| {
            if pending.operation != PendingControlOperation::SessionTitle {
                return true;
            }

            if let Some(input) = pending.input.take() {
                input.cancel();
            }

            false
        });

        self.refresh_timer();
    }

    pub(super) fn cancel_prompt(&mut self, id: &str) -> Vec<Event> {
        let mut events = Vec::new();

        if self
            .pending_approval
            .as_ref()
            .is_some_and(|pending| pending.request_id == id)
        {
            self.pending_approval = None;

            events.push(Event::ApprovalResolved);
        }

        if self
            .pending_questions
            .as_ref()
            .is_some_and(|pending| pending.request_id == id)
        {
            self.pending_questions = None;

            events.push(Event::InputResolved {
                id: id.to_owned(),
                resolution: QuestionResolution::Expired,
            });
        }

        events
    }

    pub(super) fn finish_turn(&mut self) -> Vec<Event> {
        let mut events = Vec::new();

        if self.pending_approval.take().is_some() {
            events.push(Event::ApprovalResolved);
        }

        if let Some(pending) = self.pending_questions.take() {
            events.push(Event::InputResolved {
                id: pending.request_id,
                resolution: QuestionResolution::Expired,
            });
        }

        events
    }

    pub(super) fn close(&mut self, message: &str) -> Vec<Event> {
        self.closed = true;

        let mut operations = Vec::new();

        for (_, pending) in self.pending.drain() {
            if pending.class != RequestClass::Control
                && let Some(input) = pending.input
            {
                input.cancel();
            }

            operations.push(pending.operation);
        }

        self.timer.take();

        let mut events = self.finish_turn();

        events.extend(self.effort.close(message));

        events.extend(fail_pending_control_operations(operations, message));

        events
    }

    pub(super) fn is_closed(&self) -> bool {
        self.closed
    }
}

/// A `can_use_tool` control request awaiting the user's decision. The original
/// input is kept because an allow response must echo it as `updatedInput`, and
/// the CLI's permission suggestions back the "always allow" decision.
pub(super) struct PendingApproval {
    pub(super) request_id: String,
    pub(super) input: Value,
    pub(super) suggestions: Option<Value>,
}

/// An `AskUserQuestion` request awaiting the user's picks. The CLI does not
/// generate the answers itself: it re-runs the tool with whatever the client
/// merges into `updatedInput`, so the untouched `input` is kept to echo back
/// and the parsed questions drive the card.
pub(super) struct PendingQuestions {
    pub(super) request_id: String,
    pub(super) input: Value,
    pub(super) questions: Vec<Question>,
}

/// Build the `updatedInput` an answered `AskUserQuestion` is re-run with.
/// `answers` holds chosen option labels per question, in question order.
///
/// The tool reads its answers back out of its own input, so the original
/// payload is echoed with `answers` merged in rather than replaced by an
/// answers-only object. Keys are the question texts verbatim, because that is
/// what the provider matches an answer against.
pub(super) fn merge_question_answers(
    mut input: Value,
    questions: &[Question],
    answers: Vec<Vec<String>>,
) -> Value {
    let mut answered = serde_json::Map::new();

    for (question, labels) in questions.iter().zip(answers) {
        if labels.is_empty() {
            continue;
        }

        // A single-select question is reported as the bare label; the provider
        // joins a multi-select array with ", " on its side, so the array form
        // stays the honest representation of what was picked.
        answered.insert(
            question.question.clone(),
            if question.multi_select {
                Value::Array(labels.into_iter().map(Value::String).collect())
            } else {
                Value::String(labels.into_iter().next().unwrap_or_default())
            },
        );
    }

    input["answers"] = Value::Object(answered);

    input
}

/// Read the tool's `questions` array. A question with fewer than two options
/// cannot be answered by picking, so it is dropped rather than rendered as an
/// unanswerable row; an empty result means the request is not usable at all.
pub(super) fn parse_questions(input: &Value) -> Vec<Question> {
    let Some(questions) = input["questions"].as_array() else {
        return Vec::new();
    };

    questions
        .iter()
        .filter_map(|question| {
            let options: Vec<QuestionOption> = question["options"]
                .as_array()?
                .iter()
                .filter_map(|option| {
                    Some(QuestionOption {
                        label: option["label"].as_str()?.to_owned(),
                        description: option["description"].as_str().map(str::to_owned),
                    })
                })
                .collect();

            if options.len() < 2 {
                return None;
            }

            Some(Question {
                input: Default::default(),
                header: question["header"].as_str().map(str::to_owned),
                question: question["question"].as_str()?.to_owned(),
                multi_select: question["multiSelect"].as_bool().unwrap_or(false),
                options,
            })
        })
        .collect()
}

fn operation_resolved(operation: PendingControlOperation, response: &Value) -> Option<Event> {
    let error = control_response_error(response);

    match operation {
        // The handshake answer is read by the session itself, and an effort
        // answer is settled by the ordered effort state before reaching here.
        PendingControlOperation::Init | PendingControlOperation::Effort => None,
        PendingControlOperation::Other => error.map(|message| Event::Error {
            message,
            fatal: false,
        }),
        PendingControlOperation::FileRewind => Some(Event::FileRewindCompleted { error }),
        // A composition that could not be computed leaves the previous
        // breakdown in place: the accounting beside it is still accurate, and
        // an error here says nothing about the conversation.
        PendingControlOperation::ContextComposition => error
            .is_none()
            .then(|| parse_context_composition(&response["response"]))
            .flatten()
            .map(Event::ContextCompositionUpdated),
        // The CLI answers with a null title when it had too little to name,
        // and a build that does not know the request answers with an error.
        // Neither is worth showing the user: the conversation keeps the name
        // it already had. Both are worth a line in the log, because nothing
        // else distinguishes them from a request that was never made.
        PendingControlOperation::SessionTitle => {
            let title = error
                .is_none()
                .then(|| response["response"]["title"].as_str())
                .flatten()
                .map(str::trim)
                .filter(|title| !title.is_empty());

            match (title, error) {
                (Some(title), _) => return Some(Event::TitleUpdated(title.to_owned())),
                (None, Some(error)) => debug!("claude declined to name the session: {error}"),
                (None, None) => debug!("claude named the session with nothing"),
            }

            None
        }
        PendingControlOperation::SideQuestion(id) => Some(Event::SideQuestionAnswered {
            id,
            answer: match error {
                Some(error) => Err(error),
                None => side_question_answer(&response["response"]),
            },
        }),
    }
}

/// The CLI answers with a null response when the model produced no text, so
/// an empty answer is reported as a failure rather than shown as blank.
fn side_question_answer(payload: &Value) -> Result<String, String> {
    payload["response"]
        .as_str()
        .map(str::trim)
        .filter(|answer| !answer.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| "Claude returned no answer to the side question.".to_string())
}

/// Categories the CLI lists beside its usage breakdown rather than as part of
/// it: the window still empty, and the reserve compaction keeps for itself.
/// Both are the window's free room, so listing them among the parts filling it
/// puts "Free space" at the top of what is supposedly full. The CLI's own
/// markdown export drops the same names. This version reports no field saying
/// which rows these are, so the names are the only handle on them.
const NON_USAGE_CATEGORIES: [&str; 3] = ["Free space", "Autocompact buffer", "Compact buffer"];

/// Read the CLI's context breakdown. Every field beyond the segments is
/// optional because a payload that drops one still describes the split
/// usefully, and the alternative is showing nothing.
fn parse_context_composition(payload: &Value) -> Option<ContextComposition> {
    let segments: Vec<ContextSegment> = payload["categories"]
        .as_array()?
        .iter()
        .filter_map(|category| {
            let tokens = category["tokens"].as_u64()?;
            let name = category["name"].as_str()?;

            if NON_USAGE_CATEGORIES.contains(&name) {
                return None;
            }

            Some(ContextSegment {
                label: name.to_owned(),
                tokens,
                color: category["color"].as_str().map(str::to_owned),
                deferred: category["isDeferred"].as_bool().unwrap_or(false),
            })
        })
        .collect();

    if segments.is_empty() {
        return None;
    }

    Some(ContextComposition {
        used_tokens: payload["totalTokens"]
            .as_u64()
            .unwrap_or_else(|| segments.iter().map(|segment| segment.tokens).sum()),
        max_tokens: payload["maxTokens"].as_u64().filter(|max| *max > 0),
        raw_max_tokens: payload["rawMaxTokens"].as_u64().filter(|max| *max > 0),
        auto_compact_threshold: payload["autoCompactThreshold"]
            .as_u64()
            .filter(|threshold| *threshold > 0),
        segments,
    })
}

fn fail_pending_control_operations(
    operations: impl IntoIterator<Item = PendingControlOperation>,
    message: &str,
) -> Vec<Event> {
    operations
        .into_iter()
        .filter_map(|operation| match operation {
            PendingControlOperation::Init
            | PendingControlOperation::Effort
            | PendingControlOperation::Other => None,
            PendingControlOperation::FileRewind => Some(Event::FileRewindCompleted {
                error: Some(message.to_string()),
            }),
            // Nothing is waiting on a breakdown, so a lost one is not worth
            // reporting; the next turn asks again.
            PendingControlOperation::ContextComposition => None,
            // A conversation that lost its naming request keeps the name it
            // already had, which is what an unnamed one shows anyway.
            PendingControlOperation::SessionTitle => None,
            PendingControlOperation::SideQuestion(id) => Some(Event::SideQuestionAnswered {
                id,
                answer: Err(message.to_string()),
            }),
        })
        .collect()
}

fn control_response_error(response: &Value) -> Option<String> {
    match response["subtype"].as_str() {
        Some("success") => None,
        Some("error") => Some(
            response["error"]
                .as_str()
                .unwrap_or("unknown Claude control error")
                .to_string(),
        ),
        _ => Some("Claude returned a malformed control response.".to_string()),
    }
}

struct Change {
    id: String,
    value: String,
    result: Option<ChangeResult>,
}

enum ChangeResult {
    Applied,
    Rejected(String),
    Unknown,
}

/// Responses may arrive in a different order from writes. Confirm changes in
/// submission order so an older rejection cannot undo a later accepted level.
#[derive(Default)]
struct EffortState {
    confirmed: Option<String>,
    unconfirmed: Option<String>,
    pending: VecDeque<Change>,
}

impl EffortState {
    fn new(confirmed: Option<String>) -> Self {
        Self {
            confirmed,
            unconfirmed: None,
            pending: VecDeque::new(),
        }
    }

    fn desired(&self) -> Option<&str> {
        self.pending
            .back()
            .map(|change| change.value.as_str())
            .or(self.unconfirmed.as_deref())
            .or(self.confirmed.as_deref())
    }

    fn record(&mut self, id: String, value: String) {
        self.pending.push_back(Change {
            id,
            value,
            result: None,
        });
    }

    fn contains(&self, id: &str) -> bool {
        self.pending.iter().any(|change| change.id == id)
    }

    fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    fn resolve(&mut self, id: &str, error: Option<String>) -> Option<Event> {
        let change = self.pending.iter_mut().find(|change| change.id == id)?;

        if change.result.is_some() {
            return None;
        }

        change.result = Some(error.map_or(ChangeResult::Applied, ChangeResult::Rejected));

        self.settle()
    }

    fn expire(&mut self, id: &str) -> Option<Event> {
        let change = self.pending.iter_mut().find(|change| change.id == id)?;

        if change.result.is_some() {
            return None;
        }

        // A missing acknowledgment is not a refusal. Retain the requested
        // level as uncertain so sending another prompt does not resend it.
        change.result = Some(ChangeResult::Unknown);

        self.settle()
    }

    fn close(&mut self, message: &str) -> Option<Event> {
        for change in &mut self.pending {
            if change.result.is_none() {
                change.result = Some(ChangeResult::Rejected(message.to_string()));
            }
        }

        self.settle()
    }

    fn settle(&mut self) -> Option<Event> {
        let mut errors = Vec::new();

        while self
            .pending
            .front()
            .is_some_and(|change| change.result.is_some())
        {
            let Some(change) = self.pending.pop_front() else {
                break;
            };

            match change.result {
                Some(ChangeResult::Applied) => {
                    self.confirmed = Some(change.value);
                    self.unconfirmed = None;
                }
                Some(ChangeResult::Rejected(error)) => errors.push(error),
                Some(ChangeResult::Unknown) => self.unconfirmed = Some(change.value),
                None => unreachable!("only completed changes are removed"),
            }
        }

        (!errors.is_empty()).then(|| Event::EffortRejected {
            message: errors.join("; "),
            effort: self.desired().map(str::to_owned),
        })
    }
}
