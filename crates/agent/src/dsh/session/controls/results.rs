use serde_json::Value;

use crate::chat::{Event, QuestionResolution};
use crate::dsh::session::Session;
use crate::dsh::session::controls::{Operation, question_id};

impl Session {
    pub(in crate::dsh::session) fn expire_questions(&mut self) -> Vec<Event> {
        self.controls.retire_questions();

        self.pending_questions
            .take()
            .map(|request| Event::InputResolved {
                id: question_id(&request),
                resolution: QuestionResolution::Expired,
            })
            .into_iter()
            .collect()
    }

    pub(in crate::dsh::session) fn control_completed(&mut self, payload: &Value) -> Vec<Event> {
        let Some(operation) = payload["id"]
            .as_u64()
            .and_then(|id| self.controls.complete(id))
        else {
            return Vec::new();
        };

        let error = payload["error"].as_str().map(str::to_string);
        let mut events = Vec::new();

        if let Some(message) = payload["stopError"].as_str() {
            events.push(Event::Error {
                message: format!("DeepSeek could not confirm stopping the turn: {message}"),
                fatal: false,
            });
        }

        match operation {
            Operation::Approval(request) => {
                if self.pending_approval.as_ref() == Some(&request) {
                    match error {
                        Some(message) => events.push(Event::Error {
                            message,
                            fatal: false,
                        }),

                        None => {
                            self.pending_approval = None;
                            events.push(Event::ApprovalResolved);
                        }
                    }
                }
            }

            Operation::Questions { request, skipped } => {
                if self.pending_questions.as_ref() == Some(&request) {
                    let id = question_id(&request);

                    match error {
                        Some(message) => events.push(Event::InputSubmissionFailed { id, message }),

                        None => {
                            self.pending_questions = None;

                            events.push(Event::InputResolved {
                                id,
                                resolution: if skipped {
                                    QuestionResolution::Skipped
                                } else {
                                    QuestionResolution::Submitted {
                                        message: None,
                                        started_turn: false,
                                    }
                                },
                            });
                        }
                    }
                }
            }

            Operation::Interrupt | Operation::InterruptChild(_) => {
                if let Some(message) = error {
                    events.push(Event::Error {
                        message,
                        fatal: false,
                    });
                }
            }
        }

        events
    }
}
