//! One tab's conversation on the shared host.
//!
//! Unlike the CLI-backed adapters, a session here owns no process: the host
//! outlives every tab, and this holds a session id on it plus the reader
//! threads feeding the pane.

use std::sync::Arc;

use serde_json::{Value, json};

use crate::chat::{Event, ThreadSettings};
use crate::deepseek::api::ApiClient;
use crate::deepseek::events::Downlinks;
use crate::deepseek::host::{self, Host, HostError};
use crate::deepseek::mapping::{self, ApprovalRequest, QuestionRequest, ToolTracker};
use crate::deepseek::usage::UsageTracker;

pub struct Session {
    client: ApiClient,
    session_id: String,
    /// Every open session holds the shared host, which stops when the last one
    /// drops. This is what makes the host outlive individual tabs without
    /// outliving all of them.
    host: Arc<Host>,
    /// Reader threads for both downlinks; dropping them ends the delivery.
    _downlinks: Downlinks,
    /// The turn state this side knows about, so a stop is only offered while a
    /// turn is actually running.
    running: bool,
    /// The approval the harness is currently blocked on. Held because the
    /// answer has to carry identities the transcript vocabulary does not.
    pending_approval: Option<ApprovalRequest>,
    /// The question batch the harness is currently blocked on, held for the
    /// same reason: the answer is matched against the asked question ids.
    pending_questions: Option<QuestionRequest>,
    /// Tool calls awaiting their result, so a result can complete the row its
    /// call opened rather than starting a second one.
    tools: ToolTracker,
    /// The usage projections seen so far. Each arrives as its own frame, and
    /// the pane's snapshot is assembled from more than one of them.
    usage: UsageTracker,
}

impl Session {
    /// Create a conversation on the running host and start delivering its
    /// frames. `cwd` is the project directory this tab works in; the host's own
    /// working directory applies when it is absent.
    pub fn create(
        launch: &crate::LaunchConfig,
        cwd: Option<String>,
        deliver: impl Fn(Value) + Send + Sync + 'static,
    ) -> Result<Self, HostError> {
        let host = host::shared(launch)?;
        let client = host.client().clone();
        let payload = match cwd {
            Some(cwd) => json!({ "cwd": cwd }),
            None => json!({}),
        };
        let created = client.call("session.create", payload).map_err(|error| {
            HostError::FailedToStart(format!(
                "the harness could not open a conversation: {}",
                error.message()
            ))
        })?;
        let session_id = created["sessionId"]
            .as_str()
            .ok_or_else(|| {
                HostError::FailedToStart(
                    "the harness created a conversation without an id".to_string(),
                )
            })?
            .to_string();

        // Opening the downlinks after the session exists means its first frames
        // cannot be missed: the stream replays a baseline for every attached
        // session when it opens.
        let downlinks = Downlinks::open(host.base(), Arc::downgrade(&host), deliver);

        Ok(Self {
            client,
            session_id,
            host,
            _downlinks: downlinks,
            running: false,
            pending_approval: None,
            pending_questions: None,
            tools: ToolTracker::default(),
            usage: UsageTracker::default(),
        })
    }

    /// Whether the shared host is still serving. A host that exited takes every
    /// tab's session with it, so a tab reports that rather than failing each
    /// later action on its own.
    pub fn host_is_running(&self) -> bool {
        self.host.is_running()
    }

    /// The host's own diagnostics, for explaining an exit nobody asked for.
    pub fn host_diagnostics(&self) -> String {
        self.host.stderr_tail()
    }

    /// Map one delivered frame into transcript events. Frames for other
    /// sessions and types this build does not know produce nothing.
    pub fn process(&mut self, frame: Value) -> Vec<Event> {
        // An approval is answerable, so recognizing it means recording what an
        // answer will need. The stream replays a still-pending request when it
        // reconnects, and re-raising the card from the replay is what lets a
        // tab that lost its socket mid-question still be answered.
        if let Some(request) = mapping::approval_request(&frame, &self.session_id) {
            let description = request.description.clone();
            self.pending_approval = Some(request);
            return vec![Event::ApprovalRequested { description }];
        }

        // Questions replay on reconnect exactly as approvals do, so the same
        // rule applies: recognizing the frame is what makes it answerable.
        if let Some((request, questions)) = mapping::question_request(&frame, &self.session_id) {
            self.pending_questions = Some(request);
            return vec![Event::QuestionsRequested { questions }];
        }

        // A projection frame carries one unit's whole value, and the snapshots
        // the pane renders are folded from several of them, so this is the one
        // mapping that has to remember what the earlier frames said.
        if let Some(events) = self.usage.apply(&frame, &self.session_id) {
            return events;
        }

        let events = mapping::map_frame(&frame, &self.session_id, &mut self.tools);

        for event in &events {
            match event {
                Event::TurnStarted => self.running = true,
                Event::TurnCompleted { .. } => {
                    self.running = false;
                    // A turn that ended cannot still be waiting on an answer.
                    self.pending_approval = None;
                    self.pending_questions = None;
                }
                Event::ApprovalResolved => self.pending_approval = None,
                Event::QuestionsResolved => self.pending_questions = None,
                _ => {}
            }
        }

        events
    }

    /// Answer the approval the harness is blocked on.
    ///
    /// The harness accepts only `allowed-once` and `rejected` from a client;
    /// `cancelled` and `unavailable` are outcomes it reaches on its own. So a
    /// request to allow for the rest of the session cannot be expressed, and a
    /// request to cancel the turn is a refusal plus a stop.
    pub fn respond_approval(&mut self, decision: &str) {
        let Some(request) = self.pending_approval.take() else {
            return;
        };

        let outcome = match decision {
            "accept" | "acceptForSession" => "allowed-once",
            _ => "rejected",
        };

        if let Err(error) = self.client.respond(
            &request.rpc_id,
            json!({
                "sessionId": self.session_id,
                "approvalId": request.approval_id,
                "outcome": outcome,
            }),
        ) {
            // Nothing else reports this: a refused answer leaves the turn
            // waiting exactly as an unanswered one does.
            tracing::warn!(
                "deepseek approval answer was not accepted: {}",
                error.message()
            );
        }

        if decision == "cancel" {
            self.interrupt();
        }
    }

    /// Answer the question batch the harness is blocked on, or dismiss it when
    /// `answers` is `None`.
    ///
    /// The harness validates the batch as a whole against what it asked: one
    /// answer per question, in ask order, carrying only labels it offered. So a
    /// batch that does not line up is dropped here rather than sent to be
    /// rejected, which would leave the turn waiting with the card already gone.
    pub fn respond_questions(&mut self, answers: Option<Vec<Vec<String>>>) {
        let Some(request) = self.pending_questions.take() else {
            return;
        };

        let result = match answers {
            Some(answers) if answers.len() == request.ids.len() => {
                let answers: Vec<Value> = request
                    .ids
                    .iter()
                    .zip(answers)
                    .map(|(id, selected)| json!({ "id": id, "selected": selected }))
                    .collect();
                self.client.respond(
                    &request.rpc_id,
                    json!({
                        "sessionId": self.session_id,
                        "answer": { "answers": answers },
                    }),
                )
            }
            Some(answers) => {
                tracing::warn!(
                    "deepseek question answers covered {} of {} questions and were dropped",
                    answers.len(),
                    request.ids.len(),
                );
                return;
            }
            None => self.client.respond_cancelled(&request.rpc_id),
        };

        if let Err(error) = result {
            // Nothing else reports this: a refused answer leaves the turn
            // waiting exactly as an unanswered one does.
            tracing::warn!(
                "deepseek question answer was not accepted: {}",
                error.message()
            );
        }
    }

    /// Send a prompt. Returns why it was refused, so the composer can keep the
    /// text the user typed rather than losing it to a failed send.
    pub fn send_user_message(&mut self, text: &str) -> Result<(), String> {
        self.client
            .call(
                "session.prompt",
                json!({
                    "sessionId": self.session_id,
                    "mode": "queue",
                    "content": [{ "type": "text", "text": text }],
                }),
            )
            .map(|_| ())
            .map_err(|error| error.message().to_string())
    }

    /// Stop the running turn. The harness keeps whatever the turn already
    /// streamed, so nothing is discarded here either.
    pub fn interrupt(&mut self) {
        if !self.running {
            return;
        }

        if let Err(error) = self
            .client
            .call("session.cancel", json!({ "sessionId": &self.session_id }))
        {
            tracing::warn!("deepseek turn could not be stopped: {}", error.message());
        }
    }

    pub fn session_id(&self) -> Option<&str> {
        Some(&self.session_id)
    }

    pub fn has_active_operation(&self) -> bool {
        self.running
    }

    /// The thread controls this integration can report. Model and effort
    /// selection is not mapped yet, so the pickers stay empty rather than
    /// showing a value the tab cannot change.
    pub fn initial_settings() -> ThreadSettings {
        ThreadSettings::default()
    }
}
