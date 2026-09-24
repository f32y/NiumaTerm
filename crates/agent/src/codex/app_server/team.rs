#[cfg(test)]
#[path = "team_tests.rs"]
mod team_tests;

use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::session::team_capabilities::{RecoveredTeamTurn, TeamLaunch};

pub(super) struct TeamState {
    pub(super) launch: Box<TeamLaunch>,
    pub(super) ready: bool,
    pub(super) moderation_registered: bool,
    pub(super) pending_decisions: BTreeMap<u64, String>,
    pub(super) completed_turns: Vec<RecoveredTeamTurn>,
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

pub(super) fn decision_tool() -> Value {
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
