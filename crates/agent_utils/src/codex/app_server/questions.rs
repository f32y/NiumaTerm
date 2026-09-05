//! Pending requests and answer acknowledgements belong to one root thread.

use std::collections::{HashMap, HashSet};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::chat::{
    Event, Question, QuestionInput, QuestionMode, QuestionOption, QuestionRequest,
    QuestionResolution, ThreadSettings,
};
use crate::codex::app_server::Session;
use crate::codex::app_server::protocol::{codex_user_input, turn_start_params};

#[derive(Default)]
pub(super) struct QuestionState {
    pending: HashMap<String, PendingQuestion>,
    seen_messages: HashSet<String>,
    submissions: HashMap<u64, AnswerSubmission>,
}

struct PendingQuestion {
    request: QuestionRequest,
    source: QuestionSource,
}

enum QuestionSource {
    Request {
        rpc_id: u64,
        turn_id: String,
        question_ids: Vec<String>,
        submitted: Option<QuestionResolution>,
    },
    Message,
}

struct AnswerSubmission {
    question_id: String,
    text: String,
    settings: ThreadSettings,
    steered: bool,
}

#[derive(Deserialize)]
struct AsyncQuestion {
    title: String,
    options: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct InputRequest {
    thread_id: String,
    turn_id: String,
    item_id: String,
    questions: Vec<InputQuestion>,
    is_blocking: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct InputQuestion {
    id: String,
    header: String,
    question: String,
    #[serde(default)]
    is_other: bool,
    #[serde(default)]
    is_secret: bool,
    options: Option<Vec<InputOption>>,
}

#[derive(Deserialize)]
struct InputOption {
    label: String,
    description: String,
}

pub(super) fn parse_async_questions(item: &Value) -> Option<Vec<Question>> {
    if item["delivery"].as_str() != Some("async") {
        return None;
    }
    let questions: Vec<AsyncQuestion> = serde_json::from_value(item["questions"].clone()).ok()?;
    if questions.is_empty()
        || questions.iter().any(|question| {
            question.title.trim().is_empty()
                || question.options.as_ref().is_some_and(|options| {
                    options.is_empty() || options.iter().any(|label| label.trim().is_empty())
                })
        })
    {
        return None;
    }
    Some(
        questions
            .into_iter()
            .map(|question| Question {
                header: None,
                question: question.title,
                multi_select: false,
                options: question
                    .options
                    .unwrap_or_default()
                    .into_iter()
                    .map(|label| QuestionOption {
                        label,
                        description: None,
                    })
                    .collect(),
                input: QuestionInput::Text,
            })
            .collect(),
    )
}

impl QuestionState {
    pub(super) fn has_active_request(&self) -> bool {
        !self.submissions.is_empty()
            || self
                .pending
                .values()
                .any(|pending| matches!(pending.source, QuestionSource::Request { .. }))
    }

    pub(super) fn observe_message(&mut self, item: &Value) -> Option<Event> {
        let item_id = item["id"].as_str().filter(|id| !id.is_empty())?;
        let questions = parse_async_questions(item)?;
        if !self.seen_messages.insert(item_id.to_string()) {
            return None;
        }
        let request = QuestionRequest {
            id: format!("message:{item_id}"),
            mode: QuestionMode::Async,
            questions,
        };
        self.pending.insert(
            request.id.clone(),
            PendingQuestion {
                request: request.clone(),
                source: QuestionSource::Message,
            },
        );
        Some(Event::InputRequested(request))
    }

    pub(super) fn resolve_request(&mut self, rpc_id: u64) -> Option<Event> {
        let id = format!("request:{rpc_id}");
        let pending = self.pending.remove(&id)?;
        let resolution = match pending.source {
            QuestionSource::Request { submitted, .. } => {
                submitted.unwrap_or(QuestionResolution::Expired)
            }
            QuestionSource::Message => return None,
        };
        Some(Event::InputResolved { id, resolution })
    }

    pub(super) fn end_turn(&mut self, turn_id: &str) -> Vec<Event> {
        let ids: Vec<u64> = self
            .pending
            .values()
            .filter_map(|pending| match &pending.source {
                QuestionSource::Request {
                    rpc_id,
                    turn_id: requested_turn,
                    ..
                } if requested_turn == turn_id => Some(*rpc_id),
                _ => None,
            })
            .collect();
        ids.into_iter()
            .filter_map(|id| self.resolve_request(id))
            .collect()
    }
}

impl Session {
    pub fn restore_question_requests(&mut self, requests: Vec<QuestionRequest>) {
        for request in requests {
            if request.mode != QuestionMode::Async
                || self.questions.pending.contains_key(&request.id)
            {
                continue;
            }
            let Some(item_id) = request.id.strip_prefix("message:") else {
                continue;
            };
            self.questions.seen_messages.insert(item_id.to_string());
            self.questions.pending.insert(
                request.id.clone(),
                PendingQuestion {
                    request,
                    source: QuestionSource::Message,
                },
            );
        }
    }

    pub(super) fn process_question_request(&mut self, rpc_id: u64, params: &Value) -> Vec<Event> {
        let parsed = serde_json::from_value::<InputRequest>(params.clone())
            .map_err(|error| format!("Invalid user-input request: {error}"))
            .and_then(|request| {
                let mut ids = HashSet::new();
                if self.thread_id.as_deref() != Some(request.thread_id.as_str())
                    || request.turn_id.is_empty()
                    || request.item_id.is_empty()
                    || request.questions.is_empty()
                    || request.questions.iter().any(|question| {
                        question.id.is_empty()
                            || question.question.trim().is_empty()
                            || !ids.insert(question.id.clone())
                    })
                {
                    return Err("Invalid question identity or thread".to_string());
                }
                Ok(request)
            });
        let request = match parsed {
            Ok(request) => request,
            Err(message) => {
                self.send(json!({"jsonrpc": "2.0", "id": rpc_id,
                    "error": {"code": -32602, "message": message}}));
                return Vec::new();
            }
        };
        let id = format!("request:{rpc_id}");
        if self.questions.pending.contains_key(&id) {
            return Vec::new();
        }
        let question_ids = request
            .questions
            .iter()
            .map(|question| question.id.clone())
            .collect();
        let batch = QuestionRequest {
            id: id.clone(),
            mode: if request.is_blocking.unwrap_or(true) {
                QuestionMode::Blocking
            } else {
                QuestionMode::Optional
            },
            questions: request
                .questions
                .into_iter()
                .map(|question| {
                    let options: Vec<QuestionOption> = question
                        .options
                        .unwrap_or_default()
                        .into_iter()
                        .map(|option| QuestionOption {
                            label: option.label,
                            description: Some(option.description),
                        })
                        .collect();
                    let input = if question.is_secret {
                        QuestionInput::Secret
                    } else if question.is_other || options.is_empty() {
                        QuestionInput::Text
                    } else {
                        QuestionInput::SelectionOnly
                    };
                    Question {
                        header: Some(question.header),
                        question: question.question,
                        multi_select: false,
                        options,
                        input,
                    }
                })
                .collect(),
        };
        self.questions.pending.insert(
            id,
            PendingQuestion {
                request: batch.clone(),
                source: QuestionSource::Request {
                    rpc_id,
                    turn_id: request.turn_id,
                    question_ids,
                    submitted: None,
                },
            },
        );
        vec![Event::InputRequested(batch)]
    }

    /// A successful return means an attempt was written. Resolution arrives as an event.
    pub fn respond_input(
        &mut self,
        id: &str,
        answers: Option<Vec<Vec<String>>>,
        settings: &ThreadSettings,
    ) -> Result<(), String> {
        let pending = self
            .questions
            .pending
            .get(id)
            .ok_or("This question is no longer pending")?;
        if self
            .questions
            .submissions
            .values()
            .any(|submission| submission.question_id == id)
        {
            return Err("These answers are already being submitted".to_string());
        }
        if let Some(answers) = answers.as_ref()
            && (answers.len() != pending.request.questions.len()
                || answers
                    .iter()
                    .zip(&pending.request.questions)
                    .any(|(answers, question)| {
                        answers.is_empty()
                            || answers.iter().any(|answer| answer.trim().is_empty())
                            || (!question.multi_select && answers.len() != 1)
                            || (question.input == QuestionInput::SelectionOnly
                                && answers.iter().any(|answer| {
                                    !question
                                        .options
                                        .iter()
                                        .any(|option| option.label == *answer)
                                }))
                    }))
        {
            return Err("Complete every question before submitting".to_string());
        }
        match &pending.source {
            QuestionSource::Request {
                rpc_id,
                question_ids,
                submitted,
                ..
            } => {
                if submitted.is_some() {
                    return Err("These answers are already being submitted".to_string());
                }
                let rpc_id = *rpc_id;
                let resolution = if answers.is_some() {
                    QuestionResolution::Submitted {
                        message: None,
                        started_turn: false,
                    }
                } else {
                    QuestionResolution::Skipped
                };
                let values: HashMap<&str, Value> = question_ids
                    .iter()
                    .map(String::as_str)
                    .zip(answers.unwrap_or_default())
                    .map(|(id, answers)| (id, json!({"answers": answers})))
                    .collect();
                let message =
                    json!({"jsonrpc": "2.0", "id": rpc_id, "result": {"answers": values}});
                self.try_send(message)?;
                if let Some(PendingQuestion {
                    source: QuestionSource::Request { submitted, .. },
                    ..
                }) = self.questions.pending.get_mut(id)
                {
                    *submitted = Some(resolution);
                }
                Ok(())
            }
            QuestionSource::Message => {
                let Some(answers) = answers else {
                    self.questions.pending.remove(id);
                    return Ok(());
                };
                let mut text = String::from("Answers to your questions:\n");
                for (index, (question, answers)) in
                    pending.request.questions.iter().zip(answers).enumerate()
                {
                    text.push_str(&format!(
                        "\n{}. {}\nAnswer: {}\n",
                        index + 1,
                        question.question,
                        answers.join("; ")
                    ));
                }
                let submission = AnswerSubmission {
                    question_id: id.to_string(),
                    text,
                    settings: settings.clone(),
                    steered: self.current_turn.is_some(),
                };
                self.send_question_message(submission)
            }
        }
    }

    fn send_question_message(&mut self, submission: AnswerSubmission) -> Result<(), String> {
        let thread_id = self
            .thread_id
            .as_deref()
            .ok_or("Codex is not connected")?
            .to_string();
        let input = codex_user_input(&submission.text, None, &[]);
        let (method, mut params) = if submission.steered {
            let turn_id = self
                .current_turn
                .as_deref()
                .ok_or("The active turn ended; submit again")?;
            (
                "turn/steer",
                json!({"threadId": thread_id, "expectedTurnId": turn_id, "input": input}),
            )
        } else {
            (
                "turn/start",
                turn_start_params(&thread_id, input, &submission.settings, &self.workspace),
            )
        };
        let rpc_id = self.alloc_rpc_id();
        params["clientUserMessageId"] =
            json!(format!("nmt-question-{}-{rpc_id}", self.registration_id));
        self.try_send(json!({"jsonrpc": "2.0", "id": rpc_id, "method": method, "params": params}))?;
        self.questions.submissions.insert(rpc_id, submission);
        Ok(())
    }

    pub(super) fn process_question_response(
        &mut self,
        rpc_id: u64,
        message: &Value,
    ) -> Option<Vec<Event>> {
        let submission = self.questions.submissions.remove(&rpc_id)?;
        let id = submission.question_id.clone();
        if let Some(error) = message["error"].as_object() {
            let error = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Could not submit answers")
                .to_string();
            // Only an explicit rejection proves the first attempt was not accepted.
            // Transport loss has an unknown outcome and must never trigger another send.
            if submission.steered
                && self.current_turn.is_none()
                && error == "no active turn to steer"
            {
                let retry = AnswerSubmission {
                    steered: false,
                    ..submission
                };
                return Some(match self.send_question_message(retry) {
                    Ok(()) => Vec::new(),
                    Err(message) => vec![Event::InputSubmissionFailed { id, message }],
                });
            }
            return Some(vec![Event::InputSubmissionFailed { id, message: error }]);
        }
        self.questions.pending.remove(&id);
        Some(vec![Event::InputResolved {
            id,
            resolution: QuestionResolution::Submitted {
                message: Some(submission.text),
                started_turn: !submission.steered,
            },
        }])
    }
}
