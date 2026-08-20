use std::collections::HashMap;
use std::mem::take;

use serde_json::Value;
use tracing::debug;

use crate::chat::{ContextComposition, ContextSegment, Event, Question, QuestionOption};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum PendingControlOperation {
    FileRewind,
    ContextComposition,
    SessionTitle,
    /// An effort change, carrying the level the session stays on when the CLI
    /// refuses it: ultracode the deployment does not offer, an environment
    /// variable that pins the level, or a level beyond what the model reaches.
    EffortChange(Option<String>),
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
    let error = match response["subtype"].as_str() {
        Some("success") => None,
        Some("error") => Some(
            response["error"]
                .as_str()
                .unwrap_or("unknown Claude control error")
                .to_string(),
        ),
        _ => Some("Claude returned a malformed file restore response.".to_string()),
    };

    match operation {
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
        // A level that was applied needs no announcement: the control already
        // shows it. A refusal has to reach the user, because the control shows
        // a level the session is not on until it does.
        PendingControlOperation::EffortChange(previous) => {
            error.map(|message| Event::EffortRejected {
                message,
                effort: previous,
            })
        }
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

/// Read the CLI's context breakdown. Every field beyond the segments is
/// optional because a payload that drops one still describes the split
/// usefully, and the alternative is showing nothing.
fn parse_context_composition(payload: &Value) -> Option<ContextComposition> {
    let segments: Vec<ContextSegment> = payload["categories"]
        .as_array()?
        .iter()
        .filter_map(|category| {
            let tokens = category["tokens"].as_u64()?;
            Some(ContextSegment {
                label: category["name"].as_str()?.to_owned(),
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
            PendingControlOperation::FileRewind => Some(Event::FileRewindCompleted {
                error: Some(message.to_string()),
            }),
            // Nothing is waiting on a breakdown, so a lost one is not worth
            // reporting; the next turn asks again.
            PendingControlOperation::ContextComposition => None,
            // A conversation that lost its naming request keeps the name it
            // already had, which is what an unnamed one shows anyway.
            PendingControlOperation::SessionTitle => None,
            // The process this level was meant for is gone. Its replacement
            // launches on the level the profile pins, so the control goes back
            // to what the lost session was on rather than keeping a pick that
            // no process ever accepted.
            PendingControlOperation::EffortChange(previous) => Some(Event::EffortRejected {
                message: message.to_string(),
                effort: previous,
            }),
        })
        .collect()
}
