use nmt_agent::team::discussion::ModeratorAction;
use nmt_agent::team::model::{MemberId, OperationId, StageId};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DecisionArguments {
    operation: OperationId,
    stage: StageId,
    action: DecisionAction,
    recipients: Vec<MemberId>,
}

impl DecisionArguments {
    /// The scheduling action the moderator's `team_decide` call asks for, at
    /// the stage and operation it names. A report names no recipients, so a
    /// report that lists some is malformed and asks for nothing.
    pub(super) fn moderator_action(self) -> Option<(StageId, OperationId, ModeratorAction)> {
        let action = match self.action {
            DecisionAction::Invite => ModeratorAction::Invite {
                recipients: self.recipients,
            },
            DecisionAction::Report if self.recipients.is_empty() => ModeratorAction::Report,
            DecisionAction::Report => return None,
        };

        Some((self.stage, self.operation, action))
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum DecisionAction {
    Invite,
    Report,
}
