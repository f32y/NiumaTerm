//! User commands and interaction answers for a session.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::background_task::{
    BackgroundTaskKey, BackgroundTaskLoadState, BackgroundTaskTranscriptUpdate,
};
use crate::chat::{
    Event, ForkAnchor, MessageImage, QuestionResponse, SendOutcome, SlashCommandOutcome,
};
use crate::dsh::api::ApiClient;
use crate::dsh::catalogs;
use crate::dsh::session::controls::{Operation, question_id};
use crate::dsh::session::loads::{
    load_agent_presets, load_commands, load_fork_checkpoints, load_search, load_sessions,
    load_skills, load_subagent_transcript, load_subagents, load_workflow_transcript,
};
use crate::dsh::session::switch::Target;
use crate::dsh::session::{Session, settled};

impl Session {
    /// Answer the approval the harness is blocked on.
    ///
    /// The harness accepts only `allowed-once` and `rejected` from a client;
    /// `cancelled` and `unavailable` are outcomes it reaches on its own. So a
    /// request to allow for the rest of the session cannot be expressed, and a
    /// request to cancel the turn is a refusal plus a stop.
    pub fn respond_approval(&mut self, decision: &str) -> bool {
        let Some(request) = self.pending_approval.as_ref() else {
            return false;
        };

        let outcome = match decision {
            "accept" | "acceptForSession" => "allowed-once",
            _ => "rejected",
        };

        self.controls.submit(
            Operation::Approval(request.clone()),
            "$events/result",
            json!({"clientId": request.client_id, "eventId": request.event_id,
                "outcome": {"kind": "result", "value": outcome}}),
            (decision == "cancel").then(|| self.session_id.clone()),
        )
    }

    /// Admission leaves the original request answerable until its result arrives.
    pub fn respond_input(
        &mut self,
        id: &str,
        answers: Option<Vec<Vec<String>>>,
    ) -> Result<QuestionResponse, String> {
        let request = self
            .pending_questions
            .as_ref()
            .filter(|request| question_id(request) == id)
            .ok_or("This question is no longer pending.")?;

        let skipped = answers.is_none();

        let outcome = match answers {
            Some(answers) if answers.len() == request.ids.len() => {
                let answers: Vec<Value> = request
                    .ids
                    .iter()
                    .zip(answers)
                    .map(|(id, selected)| json!({"id": id, "selected": selected}))
                    .collect();

                json!({"kind": "result", "value": {"answers": answers}})
            }
            Some(_) => return Err("Complete every question before submitting.".into()),
            None => json!({"kind": "rejected", "error": {
                "name": "Error", "code": "cancelled", "message": "the user dismissed the question"
            }}),
        };

        self.controls.submit(
            Operation::Questions {
                request: request.clone(),
                skipped,
            },
            "$events/result",
            json!({"clientId": request.client_id, "eventId": request.event_id, "outcome": outcome}),
            None,
        ).then_some(QuestionResponse::Pending)
            .ok_or_else(|| "The question response could not be queued.".to_string())
    }

    /// Ask for one workflow member's conversation.
    ///
    /// A live run reports its members as they are published, so there is
    /// nothing to poll: the read happens when a member is opened, and the run's
    /// own events say when it changed.
    pub fn request_workflow_agent_transcript(&mut self, task_id: &str, agent_id: &str) {
        load_workflow_transcript(
            self.client.clone(),
            task_id.to_string(),
            agent_id.to_string(),
            Arc::clone(&self.deliver),
        );
    }

    /// Ask the harness for a fresher child-agent catalog.
    ///
    /// The catalog is a call rather than a stream, so it reports what was true
    /// when it was asked. The counter travels with it and is what stops a slow
    /// answer from replacing a newer one.
    pub fn refresh_background_tasks(&mut self) {
        self.subagent_activity += 1;

        load_subagents(
            self.client.clone(),
            self.session_id.clone(),
            self.subagent_activity,
            Arc::clone(&self.deliver),
        );
    }

    /// Ask for one child's conversation.
    ///
    /// A child this session's catalog never named cannot be addressed: the read
    /// selects a transport by the child's kind, and only the catalog reports
    /// which kind a child is.
    ///
    /// A background job has no conversation, and the harness keeps its output
    /// behind the agent's own job tools, so its detail view is answered at once
    /// as unreadable instead of waiting on a read that never starts.
    pub fn load_background_task_transcript(&mut self, child: &str) -> Vec<Event> {
        if self.job_rows.iter().any(|row| row.key.id == child) {
            return vec![Event::BackgroundTaskTranscript {
                key: BackgroundTaskKey::deepseek(child),
                update: BackgroundTaskTranscriptUpdate::state(
                    BackgroundTaskLoadState::Unavailable {
                        message: "DeepSeek Harness shares job output only with the agent that started the job.".to_string(),
                    },
                ),
            }];
        }

        let Some(continuable) = self.subagent_modes.get(child).copied() else {
            return Vec::new();
        };

        load_subagent_transcript(
            self.client.clone(),
            self.session_id.clone(),
            child.to_string(),
            continuable,
            Arc::clone(&self.deliver),
        );

        Vec::new()
    }

    /// Stop a continuable child's current turn.
    ///
    /// The request rides the parent's durable authority rather than a live
    /// parent agent, and it acknowledges the signal rather than the child
    /// having stopped, so the row can stay visibly running for a moment.
    pub fn interrupt_background_task(&mut self, child: &str) -> bool {
        let payload = json!({
            "parentSessionId": self.session_id,
            "childSessionId": child,
            "mode": "continuable",
        });

        self.controls.submit(
            Operation::InterruptChild(child.to_string()),
            "subagents/interruptByParent",
            payload,
            None,
        )
    }

    /// Point the session at another model, optionally pinning a reasoning
    /// effort. The answer arrives as [`crate::chat::Event::ModelSelection`],
    /// carrying why the harness refused when it did, because a picker that
    /// silently keeps showing a value the session never adopted is worse than
    /// an error.
    ///
    /// An absent `effort` is how the adapter's own default is asked for, which
    /// is what a model switch wants: the levels belong to the exact model, so
    /// carrying the previous one over could pin a level this route rejects.
    pub(crate) fn select_model(&mut self, model: &str, effort: Option<&str>) {
        let (provider, id) = self.models.route(model);

        let mut payload = json!({
            "sessionId": self.session_id,
            "provider": provider,
            "model": id,
        });

        if let Some(effort) = effort {
            payload["reasoningEffort"] = json!(effort);
        }

        let client = self.client.clone();
        let deliver = Arc::clone(&self.deliver);
        let session_id = self.session_id.clone();
        let model = model.to_string();

        self.lane.run(async move {
            let command = match client.request("session/selectModel", payload).await {
                Ok(selected) => json!({
                    "kind": "modelSelected", "model": model,
                    "reasoningEffort": selected["selected"]["reasoningEffort"],
                }),
                Err(error) => json!({
                    "kind": "modelSelected", "model": model, "error": error.message(),
                }),
            };

            deliver(settled(&session_id, command));
        });
    }

    /// What the session is actually set to, for a caller restoring its pickers
    /// after a refused pick.
    pub fn selection(&self) -> (Option<&str>, Option<&str>) {
        (self.models.selected(), self.models.effort())
    }

    /// Switch this conversation's permission preset.
    ///
    /// The harness exposes the switch only as its `/permission` command, and
    /// selecting the preset already in effect records nothing, so sending a
    /// pick that already holds is harmless.
    ///
    /// Nobody typed this command: it restores a remembered pick when a
    /// conversation opens. A switch that took is already visible as the
    /// permission projection it moved, so only a refusal is reported.
    pub fn select_permission(&mut self, preset: &str) {
        let client = self.client.clone();
        let deliver = Arc::clone(&self.deliver);
        let session_id = self.session_id.clone();
        let preset = preset.to_string();

        self.lane.run(async move {
            let command = run_slash(&client, &session_id, "permission", &preset).await;

            let refused = command["error"].is_string()
                || matches!(
                    catalogs::command_outcome("permission", &preset, &command["value"]),
                    SlashCommandOutcome::Rejected { .. }
                );

            if refused {
                deliver(settled(&session_id, command));
            }
        });
    }

    /// Recompose this conversation's agent from another preset.
    ///
    /// The harness allows this only while no turn has run: the logged history
    /// was produced under the previous composition's tools, and a new one may
    /// not be able to make the calls that history records. Rather than
    /// predicting that here, the preset catalog is published again either way:
    /// naming the new preset, or the one still in force beside the harness's
    /// own reason for keeping it.
    pub fn select_agent_preset(&mut self, preset: &str) {
        let payload = json!({ "agentId": self.session_id, "agentPreset": preset });

        let client = self.client.clone();
        let deliver = Arc::clone(&self.deliver);
        let session_id = self.session_id.clone();
        let previous = self.agent_preset.clone();
        let preset = preset.to_string();

        self.lane.run(async move {
            let refusal = client
                .call("agentPresets/select", payload)
                .await
                .err()
                .map(|error| error.message().to_string());

            // A preset names the plugins the agent is built from, so the
            // commands and skills it serves are the ones that just changed.
            // Leaving the palette on the previous composition's would offer
            // entries the new agent cannot run.
            if refusal.is_none() {
                load_commands(client.clone(), session_id.clone(), Arc::clone(&deliver));

                load_skills(client.clone(), session_id.clone(), Arc::clone(&deliver));
            }

            let current = if refusal.is_none() {
                Some(preset)
            } else {
                previous
            };

            load_agent_presets(client, session_id, current, refusal, deliver);
        });
    }

    /// Send a prompt and the images it carries.
    ///
    /// A message sent while a turn is running is steered into that turn rather
    /// than queued behind it, which is what makes a correction land before the
    /// work it is correcting finishes. The harness treats a steer whose window
    /// has already closed as the next queued message, so both outcomes leave
    /// the message pending and the reply is reported as steered either way.
    ///
    /// The outcome names what was asked for. Admission is the harness's to
    /// answer, and a refusal ends the turn this opened with its reason.
    pub fn send_user_message(&mut self, text: &str, images: &[MessageImage]) -> SendOutcome {
        let steering = self.running;

        let mode = if steering { "steer" } else { "queue" };

        let payload = prompt_payload(&self.session_id, text, mode, images);

        let client = self.client.clone();
        let deliver = Arc::clone(&self.deliver);
        let session_id = self.session_id.clone();

        self.lane.run(async move {
            if let Err(error) = client.request("session/prompt", payload).await {
                deliver(settled(
                    &session_id,
                    json!({
                        "kind": "promptRefused", "steering": steering, "error": error.message(),
                    }),
                ));
            }
        });

        if steering {
            SendOutcome::Steered
        } else {
            SendOutcome::StartedTurn
        }
    }

    /// Drop one prompt the harness has accepted but not started.
    ///
    /// Answers whether the removal was requested. A message the harness has
    /// already claimed is one the transcript is about to show as sent, so a
    /// refusal republishes the inbox and the row returns.
    pub fn remove_queued_prompt(&mut self, item_id: &str) -> bool {
        let payload = json!({
            "sessionId": self.session_id,
            "itemId": item_id,
            "action": { "kind": "remove" },
        });

        let client = self.client.clone();
        let deliver = Arc::clone(&self.deliver);
        let session_id = self.session_id.clone();

        self.lane.run(async move {
            if let Err(error) = client.request("session/updateQueue", payload).await {
                tracing::warn!(
                    "deepseek queued prompt could not be removed: {}",
                    error.message()
                );

                deliver(settled(
                    &session_id,
                    json!({ "kind": "queueRemovalRefused", "error": error.message() }),
                ));
            }
        });

        true
    }

    /// Pin this conversation's title.
    ///
    /// The harness normalizes what it accepts and republishes the title it
    /// keeps, which is how the tab learns the final wording. The recent list
    /// is re-read afterwards because the row it holds for this conversation
    /// still carries the old one.
    pub fn rename(&mut self, title: &str) {
        let payload = json!({ "sessionId": self.session_id, "title": title });

        let client = self.client.clone();
        let deliver = Arc::clone(&self.deliver);
        let session_id = self.session_id.clone();
        let cwd = self.cwd.clone();

        self.lane.run(async move {
            match client.request("session/rename", payload).await {
                Ok(_) => load_sessions(client, cwd, deliver),
                Err(error) => deliver(settled(
                    &session_id,
                    json!({ "kind": "renameRefused", "error": error.message() }),
                )),
            }
        });
    }

    /// Ask which prompts this conversation can be branched in front of.
    pub fn request_fork_checkpoints(&mut self) {
        load_fork_checkpoints(
            self.client.clone(),
            self.session_id.clone(),
            Arc::clone(&self.deliver),
        );
    }

    /// Branch this conversation at `anchor` and continue in the copy.
    ///
    /// The harness cuts on whole turns: it takes the anchoring seq to mean the
    /// turn that seq falls in and keeps that turn entire, which is why the
    /// anchor names the prompt ahead of the one the branch stops at. The tab
    /// then moves to the child the same way it moves to any other
    /// conversation, so the parent is left exactly as it was.
    pub fn fork(&mut self, anchor: Option<&ForkAnchor>) -> Result<(), String> {
        let at_seq = match anchor {
            Some(ForkAnchor::DeepSeekThrough(seq)) => Some(*seq),
            Some(_) => return Err("that branch point belongs to another agent".to_string()),
            None => None,
        };

        self.switch_to(Target::BranchOf {
            session_id: self.session_id.clone(),
            at_seq,
        });

        Ok(())
    }

    /// Search the conversations this tab could resume for a phrase.
    ///
    /// The read runs off the caller's thread for the same reason the recent
    /// list does: it reaches the harness's own index and a composer that waited
    /// on it would be unusable until the answer came back.
    pub fn search_sessions(&mut self, query: &str) {
        load_search(
            self.client.clone(),
            self.cwd.clone(),
            query.to_string(),
            Arc::clone(&self.deliver),
        );
    }

    /// Run one of the harness's own commands. The outcome arrives as
    /// [`crate::chat::Event::SlashCommandResult`].
    ///
    /// The registry is reached directly rather than through a prompt: the host
    /// admits a prompt to the agent whatever it starts with, so a slash line
    /// sent that way would reach the model as text instead of running.
    pub fn execute_slash_command(&mut self, name: &str, arguments: &str) -> SlashCommandOutcome {
        let client = self.client.clone();
        let deliver = Arc::clone(&self.deliver);
        let session_id = self.session_id.clone();
        let name = name.to_string();
        let arguments = arguments.to_string();

        self.lane.run(async move {
            let command = run_slash(&client, &session_id, &name, &arguments).await;

            deliver(settled(&session_id, command));
        });

        SlashCommandOutcome::Accepted
    }

    /// Stop the running turn. The harness keeps whatever the turn already
    /// streamed, so nothing is discarded here either.
    pub fn interrupt(&mut self) -> bool {
        if !self.running {
            return false;
        }

        self.controls.submit(
            Operation::Interrupt,
            "session/cancel",
            json!({"request": {"sessionId": self.session_id}}),
            None,
        )
    }

    pub fn session_id(&self) -> Option<&str> {
        Some(&self.session_id)
    }

    pub fn has_active_operation(&self) -> bool {
        self.running
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.controls.clear();

        let mut actions: Vec<CloseAction> = self
            .queued_prompts
            .drain(..)
            .filter_map(|prompt| prompt.id)
            .map(CloseAction::RemoveQueued)
            .collect();

        if self.running {
            actions.push(CloseAction::CancelTurn);
        }

        schedule_close_actions(self.client.clone(), self.session_id.clone(), actions);
    }
}

/// Image bytes travel inline rather than by reference: the harness's
/// attachment method reads what a conversation already holds and is no route
/// for putting something into one. A model that declines image input refuses
/// the whole prompt, which is a business error the transcript reports, so
/// nothing is dropped silently to make a message fit.
fn prompt_payload(session_id: &str, text: &str, mode: &str, images: &[MessageImage]) -> Value {
    let mut content = vec![json!({ "type": "text", "text": text })];

    content.extend(images.iter().map(|image| {
        json!({
            "type": "image",
            "mediaType": image.media_type,
            "data": BASE64_STANDARD.encode(&image.bytes),
        })
    }));

    json!({
        "requestId": Uuid::new_v4().to_string(),
        "sessionId": session_id,
        "mode": mode,
        "content": content,
    })
}

/// Run one command line against the session's registry and describe the
/// harness's answer as the settled command a frame carries.
async fn run_slash(client: &ApiClient, session_id: &str, name: &str, arguments: &str) -> Value {
    let line = match arguments.trim() {
        "" => format!("/{name}"),
        arguments => format!("/{name} {arguments}"),
    };

    let mut command = json!({ "kind": "slash", "name": name, "arguments": arguments });

    match catalogs::execute_command(client, session_id, &line).await {
        Ok(value) => command["value"] = value,
        Err(error) => command["error"] = json!(error.message()),
    }

    command
}

// Remote cleanup for a tab that is releasing its Harness session.

/// Close cleanup is detached from the closing thread and should give up quickly if
/// another tab no longer keeps the shared host reachable.
const CLOSE_CALL_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CloseAction {
    RemoveQueued(String),
    CancelTurn,
}

/// Apply the remote work needed before a tab forgets its session. Queue entries
/// are removed before the active turn is cancelled because the Harness keeps
/// its inbox on cancellation and would otherwise start another invisible turn.
pub(crate) async fn run_close_actions(
    client: &ApiClient,
    session_id: &str,
    actions: &[CloseAction],
) -> Vec<String> {
    let mut failures = Vec::new();

    for action in actions {
        let (method, payload) = match action {
            CloseAction::RemoveQueued(item_id) => (
                "session/updateQueue",
                json!({
                    "sessionId": session_id,
                    "itemId": item_id,
                    "action": { "kind": "remove" },
                }),
            ),
            CloseAction::CancelTurn => ("session/cancel", json!({ "sessionId": session_id })),
        };

        if let Err(error) = client
            .call_with_timeout(method, json!({ "request": payload }), CLOSE_CALL_TIMEOUT)
            .await
        {
            failures.push(format!("{method}: {}", error.message()));
        }
    }

    failures
}

fn schedule_close_actions(client: ApiClient, session_id: String, actions: Vec<CloseAction>) {
    if actions.is_empty() {
        return;
    }

    nmt_runtime::handle().spawn(async move {
        for failure in run_close_actions(&client, &session_id, &actions).await {
            tracing::warn!("deepseek session close cleanup failed: {failure}");
        }
    });
}
