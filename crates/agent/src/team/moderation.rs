use crate::team::identity::{AttemptId, MemberId, OperationId};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", deny_unknown_fields, rename_all = "snake_case")]
pub enum ModeratorAction {
    Invite { recipients: Vec<MemberId> },
    Report,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModeratorDecision {
    pub operation: OperationId,
    pub attempt: AttemptId,
    pub actor: MemberId,
    pub action: ModeratorAction,
}
