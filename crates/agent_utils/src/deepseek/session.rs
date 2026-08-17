//! One tab's conversation on the shared host.
//!
//! Unlike the CLI-backed adapters, a session here owns no process: the host
//! outlives every tab, and this holds a session id on it plus the reader
//! threads feeding the pane.

use std::sync::Arc;
use std::thread;

use serde_json::{Value, json};

use crate::chat::{Event, ThreadSettings};
use crate::deepseek::api::ApiClient;
use crate::deepseek::events::Downlinks;
use crate::deepseek::host::{self, Host, HostError};
use crate::deepseek::mapping::{self, ApprovalRequest, QuestionRequest, ToolTracker};
use crate::deepseek::models::ModelDirectory;
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
    /// What the picker offers, and what a pick from it addresses. Empty until
    /// the catalog arrives, which is a background call rather than part of
    /// opening the conversation.
    models: ModelDirectory,
}

/// Frame type this adapter mints locally to carry the model catalog.
///
/// The catalog is a unary call, not a downlink push, but the pane only reacts
/// to what arrives on the delivery channel — a result parked anywhere else
/// would sit unread until some unrelated frame happened to wake the tab. The
/// `nmt/` prefix keeps it out of the harness's own type space.
const MODELS_FRAME: &str = "nmt/models";

/// Read the session's model directory and reconcile it with the profile's pick,
/// then deliver the result.
///
/// This runs off the create path because provider lookups reach the network,
/// and a tab must not wait on a slow provider before it can be typed in. The
/// selection is applied here rather than reported and applied later, so what
/// the pane displays is what the harness will actually route.
fn load_models(
    client: ApiClient,
    session_id: String,
    wanted_model: Option<String>,
    wanted_effort: Option<String>,
    deliver: Arc<impl Fn(Value) + Send + Sync + 'static>,
) {
    thread::spawn(move || {
        let mut catalog =
            match client.call("session.models", json!({ "sessionId": session_id.clone() })) {
                Ok(catalog) => catalog,
                Err(error) => {
                    tracing::warn!(
                        "deepseek model directory could not be read: {}",
                        error.message()
                    );
                    return;
                }
            };

        if let Some(model) = wanted_model {
            let directory = ModelDirectory::parse(&catalog);
            let already = directory.selected() == Some(model.as_str())
                && (wanted_effort.is_none() || directory.effort() == wanted_effort.as_deref());

            // A profile naming a model this deployment does not serve leaves
            // the harness on its own selection, which the catalog already
            // describes; overriding it with a route that does not exist would
            // only fail the next turn.
            if let Some((provider, id)) = directory.route(&model)
                && !already
            {
                let mut payload = json!({
                    "sessionId": session_id,
                    "provider": provider,
                    "model": id,
                });
                if let Some(effort) = &wanted_effort {
                    payload["reasoningEffort"] = json!(effort);
                }

                match client.call("session.selectModel", payload) {
                    Ok(selected) => catalog["current"] = selected["selected"].clone(),
                    Err(error) => tracing::warn!(
                        "deepseek could not select the profile's model: {}",
                        error.message()
                    ),
                }
            }
        }

        deliver(json!({
            "payload": { "type": MODELS_FRAME, "sessionId": session_id, "models": catalog },
        }));
    });
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
        let deliver = Arc::new(deliver);
        let downlinks = {
            let deliver = Arc::clone(&deliver);
            Downlinks::open(host.base(), Arc::downgrade(&host), move |frame| {
                deliver(frame)
            })
        };

        load_models(
            client.clone(),
            session_id.clone(),
            launch.model.clone(),
            launch.effort.clone(),
            deliver,
        );

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
            models: ModelDirectory::default(),
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

        if frame["payload"]["type"] == MODELS_FRAME {
            self.models = ModelDirectory::parse(&frame["payload"]["models"]);
            // The catalog and the selection travel together, so the pickers
            // gain their options and their current value in one repaint.
            return vec![
                Event::Models(self.models.catalog()),
                Event::Ready(ThreadSettings {
                    model: self.models.selected().map(str::to_string),
                    effort: self.models.effort().map(str::to_string),
                    ..ThreadSettings::default()
                }),
            ];
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

    /// Point the session at another model, optionally pinning a reasoning
    /// effort. Returns why the harness refused, because a picker that silently
    /// keeps showing a value the session never adopted is worse than an error.
    ///
    /// An absent `effort` is how the adapter's own default is asked for, which
    /// is what a model switch wants: the levels belong to the exact model, so
    /// carrying the previous one over could pin a level this route rejects.
    pub fn select_model(&mut self, model: &str, effort: Option<&str>) -> Result<(), String> {
        let Some((provider, id)) = self.models.route(model) else {
            return Err(format!("{model} is not one of this session's models"));
        };

        let mut payload = json!({
            "sessionId": self.session_id,
            "provider": provider,
            "model": id,
        });
        if let Some(effort) = effort {
            payload["reasoningEffort"] = json!(effort);
        }

        let selected = self
            .client
            .call("session.selectModel", payload)
            .map_err(|error| error.message().to_string())?;

        // The harness answers with the selection it committed, which is what
        // the directory records: an effort it declined to pin is absent there.
        self.models.set_selected(
            model.to_string(),
            selected["selected"]["reasoningEffort"]
                .as_str()
                .map(str::to_string),
        );
        Ok(())
    }

    /// What the session is actually set to, for a caller restoring its pickers
    /// after a refused pick.
    pub fn selection(&self) -> (Option<&str>, Option<&str>) {
        (self.models.selected(), self.models.effort())
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
}
