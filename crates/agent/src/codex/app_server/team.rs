#[cfg(test)]
mod tests;

use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::chat::TeamDecisionRequest;
use crate::codex::app_server::control::QueryKind;
use crate::codex::app_server::protocol::initial_thread_request;
use crate::codex::app_server::{ConversationStart, Event, Session};
use crate::session::AgentKind;
use crate::session::team_capabilities::{ModeratorAdmission, TeamCapabilities, TeamLaunch};
use crate::session::team_recovery::RecoveredTeamTurn;
use crate::{AgentWorkspace, LaunchConfig};

pub(super) struct TeamState {
    pub(super) launch: Box<TeamLaunch>,
    ready: bool,
    moderation_registered: bool,
    pub(super) pending_decisions: BTreeMap<u64, String>,
    completed_turns: Vec<RecoveredTeamTurn>,
}

impl TeamState {
    pub(super) fn new(launch: TeamLaunch) -> Self {
        Self {
            launch: Box::new(launch),
            ready: false,
            moderation_registered: false,
            pending_decisions: BTreeMap::new(),
            completed_turns: Vec::new(),
        }
    }

    pub(super) fn can_send(&self) -> bool {
        self.ready
    }
}

impl Session {
    pub fn team_recovered_turns(&self) -> &[RecoveredTeamTurn] {
        self.team
            .as_ref()
            .map_or(&[], |team| team.completed_turns.as_slice())
    }

    pub(super) fn retain_team_history(&mut self, turns: &Value) {
        let Some(team) = self.team.as_mut() else {
            return;
        };

        team.completed_turns = turns
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|turn| {
                if turn["status"].as_str() != Some("completed") {
                    return None;
                }

                let id = turn["id"].as_str().filter(|id| !id.is_empty())?;

                let text = turn["items"]
                    .as_array()?
                    .iter()
                    .rev()
                    .find(|item| item["type"].as_str() == Some("agentMessage"))
                    .and_then(|item| item["text"].as_str())
                    .unwrap_or_default();

                Some(RecoveredTeamTurn {
                    id: id.to_owned(),
                    text: text.to_owned(),
                })
            })
            .collect();
    }

    pub fn spawn_team(
        launch: &LaunchConfig,
        host_catalog: &[LaunchConfig],
        workspace: &AgentWorkspace,
        resume: Option<String>,
        policy: TeamLaunch,
        deliver: impl Fn(Value) + Send + Sync + 'static,
        on_stderr: impl Fn(String) + Send + 'static,
    ) -> Result<Self, String> {
        Self::spawn_inner(
            launch,
            host_catalog,
            workspace,
            ConversationStart {
                resume,
                suppress_replay: !policy.restore_transcript,
                team: Some(policy),
            },
            deliver,
            on_stderr,
        )
    }

    pub fn team_capabilities(&self, backend_generation: u64) -> TeamCapabilities {
        let mut capabilities = TeamCapabilities::unverified(AgentKind::Codex);

        if self
            .team
            .as_ref()
            .is_some_and(|team| team.ready && team.moderation_registered)
        {
            capabilities.moderation = ModeratorAdmission::CodexDynamicTools { backend_generation };
        }

        capabilities
    }

    pub(super) fn start_initial_thread(&mut self) {
        let mut request = initial_thread_request(
            self.initial_resume.as_deref(),
            &self.thread_profile,
            &self.workspace,
        );

        if self.team.as_ref().is_some_and(|team| team.launch.moderator)
            && self.initial_resume.is_none()
        {
            request["params"]["dynamicTools"] = json!([decision_tool()]);
        }

        let kind = if self.initial_resume.is_some() {
            QueryKind::Resume
        } else {
            QueryKind::Start
        };

        self.send_query(kind, request);
    }

    pub(super) fn finish_team_start(&mut self, events: Vec<Event>) -> Vec<Event> {
        if let Some(team) = &mut self.team {
            team.moderation_registered = team.launch.moderator;
            team.ready = true;
        }

        events
    }

    pub(super) fn process_team_decision(&mut self, request_id: u64, params: &Value) -> Vec<Event> {
        let valid = params["tool"].as_str() == Some("team_decide")
            && params["threadId"].as_str() == self.thread_id()
            && params["turnId"].as_str() == self.conversation.current_turn.as_deref()
            && self.team.as_ref().is_some_and(|team| {
                team.ready
                    && team.moderation_registered
                    && !team.pending_decisions.contains_key(&request_id)
            });

        if !valid {
            self.send(json!({"id": request_id, "result": {"success": false, "contentItems": [{"type": "inputText", "text": "This discussion operation is unavailable for this session or turn."}]}}));

            return Vec::new();
        }

        let Some(turn) = params["turnId"].as_str() else {
            return Vec::new();
        };

        if let Some(team) = &mut self.team {
            team.pending_decisions.insert(request_id, turn.to_owned());
        }

        vec![Event::TeamDecision(TeamDecisionRequest {
            request_id,
            provider_turn: turn.to_owned(),
            arguments: params["arguments"].clone(),
        })]
    }

    pub fn respond_team_decision(
        &mut self,
        request: &TeamDecisionRequest,
        accepted: bool,
        explanation: &str,
    ) -> bool {
        let current = self
            .team
            .as_ref()
            .and_then(|team| team.pending_decisions.get(&request.request_id));

        if current != Some(&request.provider_turn)
            || self.conversation.current_turn.as_deref() != Some(request.provider_turn.as_str())
        {
            return false;
        }

        if self.try_send(json!({"id": request.request_id, "result": {"success": accepted, "contentItems": [{"type": "inputText", "text": explanation}]}})).is_err() {
            return false;
        }

        if let Some(team) = &mut self.team {
            team.pending_decisions.remove(&request.request_id);
        }

        true
    }
}

fn decision_tool() -> Value {
    json!({
        "type": "function", "name": "team_decide", "deferLoading": false,
        "description": "Make the single scheduling decision for your current moderator turn. Invite selected members or end the discussion with a report. Use the operation and stage identifiers supplied by the application.",
        "inputSchema": {"type": "object", "additionalProperties": false,
            "properties": {
                "operation": {"type": "string"}, "stage": {"type": "string"},
                "action": {"type": "string", "enum": ["invite", "report"]},
                "recipients": {"type": "array", "items": {"type": "string"}, "uniqueItems": true}
            }, "required": ["operation", "stage", "action", "recipients"]
        }
    })
}
