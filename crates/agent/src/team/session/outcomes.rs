use crate::team::identity::{AttemptId, MemberId, OwnershipGeneration};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AttemptEventKey {
    pub attempt: AttemptId,
    pub member: MemberId,
    pub ownership: OwnershipGeneration,
    pub backend_generation: u64,
}
