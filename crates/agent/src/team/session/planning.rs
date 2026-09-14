use crate::team::attempt::BudgetScope;
use crate::team::budget::TurnPurpose;
use crate::team::content::{Author, PublicMessage, Publication, UserInput};
use crate::team::discussion::StageKind;
use crate::team::identity::{MemberId, MessageId, OperationId, StageId};

pub(super) struct DispatchPlan {
    pub(super) recipient: MemberId,
    pub(super) operation: OperationId,
    pub(super) stage: Option<(StageId, StageKind)>,
    pub(super) budget: BudgetScope,
    pub(super) purpose: TurnPurpose,
}

pub(super) fn public_request(input: UserInput) -> PublicMessage {
    PublicMessage {
        id: MessageId::new(),
        author: Author::User,
        publication: Publication::UserInput,
        text: input.text,
        replies_to: input.references,
        attachments: input.attachments,
    }
}
