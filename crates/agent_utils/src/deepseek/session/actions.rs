//! User commands and interaction answers for a session.

use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::chat::{ForkAnchor, MessageImage, SendOutcome, SlashCommandOutcome};
use crate::deepseek::api::CallError;
use crate::deepseek::close::{CloseAction, schedule_close_actions};
use crate::deepseek::commands;
use crate::deepseek::session::Session;
use crate::deepseek::session::loads::{
    load_commands, load_fork_checkpoints, load_search, load_sessions, load_skills,
    load_subagent_transcript, load_subagents, load_workflow_transcript,
};

impl Session {
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

        if let Err(error) = self.client.respond_event(
            &request.client_id,
            &request.event_id,
            json!({ "kind": "result", "value": outcome }),
        ) {
            // Nothing else reports this: a refused answer leaves the turn
            // waiting exactly as an unanswered one does.
            tracing::warn!(
                "deepseek approval answer was not accepted: {}",
                error.message()
            );
            self.pending_approval = Some(request);
        } else {
            (self.deliver)(
                json!({ "payload": { "type": "approval/resolved", "sessionId": self.session_id } }),
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
                self.client.respond_event(
                    &request.client_id,
                    &request.event_id,
                    json!({ "kind": "result", "value": { "answers": answers } }),
                )
            }
            Some(answers) => {
                tracing::warn!(
                    "deepseek question answers covered {} of {} questions and were dropped",
                    answers.len(),
                    request.ids.len(),
                );
                self.pending_questions = Some(request);
                return;
            }
            None => self.client.respond_event(&request.client_id, &request.event_id, json!({
                "kind": "rejected",
                "error": { "name": "Error", "code": "cancelled", "message": "the user dismissed the question" },
            })),
        };

        if let Err(error) = result {
            // Nothing else reports this: a refused answer leaves the turn
            // waiting exactly as an unanswered one does.
            tracing::warn!(
                "deepseek question answer was not accepted: {}",
                error.message()
            );
            self.pending_questions = Some(request);
        } else {
            (self.deliver)(
                json!({ "payload": { "type": "question/resolved", "sessionId": self.session_id } }),
            );
        }
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
    pub fn load_background_task_transcript(&mut self, child: &str) {
        let Some(continuable) = self.subagent_modes.get(child).copied() else {
            return;
        };

        load_subagent_transcript(
            self.client.clone(),
            self.session_id.clone(),
            child.to_string(),
            continuable,
            Arc::clone(&self.deliver),
        );
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

        match self.client.call("subagents/interruptByParent", payload) {
            Ok(_) => true,
            Err(error) => {
                tracing::warn!(
                    "deepseek child agent could not be stopped: {}",
                    error.message()
                );
                false
            }
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
        let (provider, id) = self.models.route(model);

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
            .request("session/selectModel", payload)
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

    /// Recompose this conversation's agent from another preset.
    ///
    /// The harness allows this only while no turn has run: the logged history
    /// was produced under the previous composition's tools, and a new one may
    /// not be able to make the calls that history records. Rather than
    /// predicting that here, the refusal is returned for the picker to show —
    /// the harness owns the rule and answers with its own reason.
    pub fn select_agent_preset(&mut self, preset: &str) -> Result<(), String> {
        let payload = json!({ "agentId": self.session_id, "agentPreset": preset });

        self.client
            .call("agentPresets/select", payload)
            .map_err(|error| error.message().to_string())?;

        // A preset names the plugins the agent is built from, so the commands
        // and skills it serves are the ones that just changed. Leaving the
        // palette on the previous composition's would offer entries the new
        // agent cannot run.
        load_commands(
            self.client.clone(),
            self.session_id.clone(),
            Arc::clone(&self.deliver),
        );
        load_skills(
            self.client.clone(),
            self.session_id.clone(),
            Arc::clone(&self.deliver),
        );

        Ok(())
    }

    /// Send a prompt and the images it carries.
    ///
    /// A message sent while a turn is running is steered into that turn rather
    /// than queued behind it, which is what makes a correction land before the
    /// work it is correcting finishes. The harness treats a steer whose window
    /// has already closed as the next queued message, so both outcomes leave
    /// the message pending and the reply is reported as steered either way.
    pub fn send_user_message(&mut self, text: &str, images: &[MessageImage]) -> SendOutcome {
        let steering = self.running;
        let mode = if steering { "steer" } else { "queue" };

        match self.prompt(text, mode, images) {
            Ok(_) if steering => SendOutcome::Steered,
            Ok(_) => SendOutcome::StartedTurn,
            Err(error) => SendOutcome::Rejected {
                message: error.message().to_string(),
            },
        }
    }

    /// Drop one prompt the harness has accepted but not started.
    ///
    /// Returns whether the harness took the removal, because a message it has
    /// already claimed is one the transcript is about to show as sent and the
    /// row must not disappear as though it never was.
    pub fn remove_queued_prompt(&mut self, item_id: &str) -> bool {
        let payload = json!({
            "sessionId": self.session_id,
            "itemId": item_id,
            "action": { "kind": "remove" },
        });

        match self.client.request("session/updateQueue", payload) {
            Ok(_) => {
                self.queued_prompt_ids.retain(|queued| queued != item_id);
                true
            }
            Err(error) => {
                tracing::warn!(
                    "deepseek queued prompt could not be removed: {}",
                    error.message()
                );
                false
            }
        }
    }

    /// Pin this conversation's title.
    ///
    /// The harness normalizes what it accepts and regenerates a title it chose
    /// itself, so the accepted title is read back rather than assumed. The
    /// recent list is re-read afterwards because the row it holds for this
    /// conversation still carries the old one.
    pub fn rename(&mut self, title: &str) -> Result<String, String> {
        let payload = json!({ "sessionId": self.session_id, "title": title });
        let renamed = self
            .client
            .request("session/rename", payload)
            .map_err(|error| error.message().to_string())?;

        load_sessions(
            self.client.clone(),
            self.cwd.clone(),
            Arc::clone(&self.deliver),
        );

        Ok(renamed["title"]
            .as_str()
            .unwrap_or(title)
            .trim()
            .to_string())
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
    /// anchor names the prompt ahead of the one the branch stops at. Omitting
    /// it falls back to the last completed turn. The tab then moves to the
    /// child the same way it moves to any other conversation, so the parent is
    /// left exactly as it was.
    pub fn fork(&mut self, anchor: Option<&ForkAnchor>) -> Result<(), String> {
        let at_seq = match anchor {
            Some(ForkAnchor::DeepSeekThrough(seq)) => Some(*seq),
            Some(_) => return Err("that branch point belongs to another agent".to_string()),
            None => None,
        };

        let mut payload = json!({ "sessionId": self.session_id });
        if let Some(at_seq) = at_seq {
            payload["atSeq"] = json!(at_seq);
        }

        let forked = self
            .client
            .request("session/fork", payload)
            .map_err(|error| error.message().to_string())?;

        let child = forked["sessionId"]
            .as_str()
            .ok_or_else(|| "the harness answered without a conversation id".to_string())?
            .to_string();

        self.resume_thread(&child)
            .then_some(())
            .ok_or_else(|| "the harness would not open the branched conversation".to_string())
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

    /// Run one of the harness's own commands.
    ///
    /// The registry is reached directly rather than through a prompt: the host
    /// admits a prompt to the agent whatever it starts with, so a slash line
    /// sent that way would reach the model as text instead of running.
    pub fn execute_slash_command(&mut self, name: &str, arguments: &str) -> SlashCommandOutcome {
        let line = match arguments.trim() {
            "" => format!("/{name}"),
            arguments => format!("/{name} {arguments}"),
        };

        let answer = self.client.call(
            commands::EXECUTE_METHOD,
            commands::execute_args(&self.session_id, &line),
        );

        match answer {
            Ok(value) => commands::outcome(name, &value),
            Err(error) => SlashCommandOutcome::Rejected {
                message: error.message().to_string(),
            },
        }
    }

    /// Image bytes travel inline rather than by reference: the harness's
    /// attachment method reads what a conversation already holds and is no
    /// route for putting something into one. A model that declines image
    /// input refuses the whole prompt, which is a business error the composer
    /// reports, so nothing is dropped silently to make a message fit.
    fn prompt(&self, text: &str, mode: &str, images: &[MessageImage]) -> Result<Value, CallError> {
        let mut content = vec![json!({ "type": "text", "text": text })];
        content.extend(images.iter().map(|image| {
            json!({
                "type": "image",
                "mediaType": image.media_type,
                "data": BASE64_STANDARD.encode(&image.bytes),
            })
        }));

        self.client.request(
            "session/prompt",
            json!({
                "requestId": Uuid::new_v4().to_string(),
                "sessionId": self.session_id,
                "mode": mode,
                "content": content,
            }),
        )
    }

    /// Stop the running turn. The harness keeps whatever the turn already
    /// streamed, so nothing is discarded here either.
    pub fn interrupt(&mut self) {
        if !self.running {
            return;
        }

        if let Err(error) = self
            .client
            .request("session/cancel", json!({ "sessionId": &self.session_id }))
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

impl Drop for Session {
    fn drop(&mut self) {
        let mut actions: Vec<CloseAction> = self
            .queued_prompt_ids
            .drain(..)
            .map(CloseAction::RemoveQueued)
            .collect();
        if self.running {
            actions.push(CloseAction::CancelTurn);
        }
        schedule_close_actions(self.client.clone(), self.session_id.clone(), actions);
    }
}
