use nmt_agent::team::identity::{MemberId, OperationId, StageId};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DecisionArguments {
    pub(super) operation: OperationId,
    pub(super) stage: StageId,
    pub(super) action: DecisionAction,
    pub(super) recipients: Vec<MemberId>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum DecisionAction {
    Invite,
    Report,
}
