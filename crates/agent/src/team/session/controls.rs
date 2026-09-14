use crate::team::attempt::{AttemptState, BudgetScope};
use crate::team::identity::DiscussionId;
use crate::team::room::Room;
use crate::team::session::TeamError;

pub(super) fn cancel_pending_reservations(
    room: &mut Room,
    id: DiscussionId,
) -> Result<(), TeamError> {
    let discussion = room
        .discussions
        .iter_mut()
        .find(|run| run.id == id)
        .ok_or(TeamError::Unavailable)?;

    for attempt in &mut room.attempts {
        if attempt.intent.budget == BudgetScope::Discussion(id)
            && attempt.state == AttemptState::Reserved
        {
            discussion.budget.cancel_unsent(attempt.id)?;

            attempt.state = AttemptState::Rejected;
        }
    }

    Ok(())
}
