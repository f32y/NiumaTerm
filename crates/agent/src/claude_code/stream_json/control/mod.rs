use std::collections::{HashMap, VecDeque};
use std::mem::take;
use std::time::Instant;

use serde_json::{Value, json};
use tracing::debug;

use crate::chat::{ContextComposition, ContextSegment, Event, Question, QuestionOption};
use crate::deadline_timer::DeadlineTimer;
use crate::request_policy::RequestClass;
use crate::subprocess::InputTicket;

struct Deadline {
    at: Instant,
    class: RequestClass,
    input: Option<InputTicket>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum PendingControlOperation {
    Other,
    FileRewind,
    ContextComposition,
    SessionTitle,
}

pub(super) struct ControlState {
    timer: Option<DeadlineTimer>,
    next_request_id: u64,
    operations: HashMap<String, PendingControlOperation>,
    deadlines: HashMap<String, Deadline>,
    closed: bool,
    effort: EffortState,
    pub(super) pending_approval: Option<PendingApproval>,
    pub(super) pending_questions: Option<PendingQuestions>,
}

impl Default for ControlState {
    fn default() -> Self {
        Self {
            timer: None,
            next_request_id: 1,
            operations: HashMap::new(),
            deadlines: HashMap::new(),
            closed: false,
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
            timer.set(self.deadlines.values().map(|deadline| deadline.at).min());
        }
    }

    pub(super) fn check_connected(&self) -> Result<(), String> {
        if self.closed {
            return Err("Claude is not connected".into());
        }

        Ok(())
    }

    pub(super) fn record_admitted(&mut self, id: String, class: RequestClass, now: Instant) {
        self.deadlines.insert(
            id,
            Deadline {
                at: now + class.timeout(),
                class,
                input: None,
            },
        );

        self.refresh_timer();
    }

    pub(super) fn attach_input(&mut self, id: &str, ticket: InputTicket) {
        if let Some(deadline) = self.deadlines.get_mut(id) {
            deadline.input = Some(ticket);
        }
    }

    pub(super) fn complete(&mut self, id: &str) {
        self.deadlines.remove(id);
        self.refresh_timer();
    }

    pub(super) fn expired(
        &mut self,
        now: Instant,
    ) -> Vec<(String, RequestClass, Option<InputTicket>)> {
        let expired = self
            .deadlines
            .extract_if(|_, deadline| deadline.at <= now)
            .map(|(id, deadline)| (id, deadline.class, deadline.input))
            .collect();

        self.refresh_timer();

        expired
    }

    pub(super) fn expire_effort(&mut self, id: &str) -> Option<Vec<Event>> {
        self.effort
            .contains(id)
            .then(|| self.effort.expire(id).into_iter().collect())
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
        if !self.closed {
            self.operations.remove(&id);
            self.effort.record(id, value);
        }
    }

    pub(super) fn request(&mut self, request: Value) -> (String, Value) {
        let request_id = format!("nmt-{}", self.next_request_id);

        self.next_request_id += 1;

        let message = json!({
            "type": "control_request", "request_id": request_id, "request": request,
        });

        (request_id, message)
    }

    pub(super) fn track(&mut self, id: String, operation: PendingControlOperation) {
        if !self.closed {
            self.operations.insert(id, operation);
        }
    }

    pub(super) fn contains(&self, operation: &PendingControlOperation) -> bool {
        self.operations.values().any(|pending| pending == operation)
    }

    pub(super) fn has_active_request(&self) -> bool {
        self.effort.has_pending()
            || self.pending_approval.is_some()
            || self.pending_questions.is_some()
            || self
                .operations
                .values()
                .any(|operation| !matches!(operation, PendingControlOperation::Other))
    }

    pub(super) fn resolve(&mut self, response: &Value) -> Option<Event> {
        if let Some(id) = response["request_id"].as_str() {
            self.complete(id);
        }

        if let Some(id) = response["request_id"].as_str()
            && self.effort.contains(id)
        {
            return self.effort.resolve(id, control_response_error(response));
        }

        resolve_pending_control_operation(&mut self.operations, response)
    }

    pub(super) fn cancel_generated_title(&mut self) {
        self.operations.retain(|id, operation| {
            if matches!(operation, PendingControlOperation::SessionTitle) {
                if let Some(deadline) = self.deadlines.remove(id)
                    && let Some(input) = deadline.input
                {
                    input.cancel();
                }

                false
            } else {
                true
            }
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
            events.push(Event::QuestionsResolved);
        }

        events
    }

    pub(super) fn finish_turn(&mut self) -> Vec<Event> {
        let mut events = Vec::new();

        if self.pending_approval.take().is_some() {
            events.push(Event::ApprovalResolved);
        }

        if self.pending_questions.take().is_some() {
            events.push(Event::QuestionsResolved);
        }

        events
    }

    pub(super) fn close(&mut self, message: &str) -> Vec<Event> {
        self.closed = true;

        for deadline in self.deadlines.drain().map(|(_, deadline)| deadline) {
            if deadline.class != RequestClass::Control
                && let Some(input) = deadline.input
            {
                input.cancel();
            }
        }

        self.timer.take();

        let mut events = self.finish_turn();

        events.extend(self.effort.close(message));

        events.extend(fail_pending_control_operations(
            &mut self.operations,
            message,
        ));

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

pub(super) fn resolve_pending_control_operation(
    pending: &mut HashMap<String, PendingControlOperation>,
    response: &Value,
) -> Option<Event> {
    let request_id = response["request_id"].as_str()?;
    let operation = pending.remove(request_id)?;
    let error = control_response_error(response);

    match operation {
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
    }
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

pub(super) fn fail_pending_control_operations(
    pending: &mut HashMap<String, PendingControlOperation>,
    message: &str,
) -> Vec<Event> {
    let operations = take(pending);

    operations
        .into_values()
        .filter_map(|operation| match operation {
            PendingControlOperation::Other => None,

            PendingControlOperation::FileRewind => Some(Event::FileRewindCompleted {
                error: Some(message.to_string()),
            }),

            // Nothing is waiting on a breakdown, so a lost one is not worth
            // reporting; the next turn asks again.
            PendingControlOperation::ContextComposition => None,
            // A conversation that lost its naming request keeps the name it
            // already had, which is what an unnamed one shows anyway.
            PendingControlOperation::SessionTitle => None,
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

        _ => Some("Claude returned a malformed file restore response.".to_string()),
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

#[cfg(test)]
mod effort_tests;
