use crate::team::attempt::{AttemptState, BudgetScope};
use crate::team::discussion::{
    Arrangement, ArrangementState, DiscussionMode, DiscussionState, PauseReason, Stage, StageKind,
};
use crate::team::identity::{DiscussionId, OperationId, StageId};
use crate::team::room::Room;
use crate::team::session::{TeamError, TeamSession};

impl TeamSession {
    pub fn pause_discussion(
        &mut self,
        id: DiscussionId,
        reason: PauseReason,
    ) -> Result<bool, TeamError> {
        let mut room = self.room().clone();

        let changed = room
            .discussions
            .iter_mut()
            .find(|run| run.id == id)
            .ok_or(TeamError::Unavailable)?
            .pause(reason);

        self.store.commit(room)?;

        Ok(changed)
    }

    pub fn resolve_pause(
        &mut self,
        id: DiscussionId,
        reason: &PauseReason,
    ) -> Result<bool, TeamError> {
        let mut room = self.room().clone();

        let changed = room
            .discussions
            .iter_mut()
            .find(|run| run.id == id)
            .ok_or(TeamError::Unavailable)?
            .resolve_pause(reason);

        self.store.commit(room)?;

        Ok(changed)
    }

    pub fn continue_discussion(&mut self, id: DiscussionId) -> Result<(), TeamError> {
        if !self.slots.is_idle() || !self.restored_uncertainty.is_empty() {
            return Err(TeamError::Busy);
        }

        let mut room = self.room().clone();

        let index = room
            .discussions
            .iter()
            .position(|run| run.id == id)
            .ok_or(TeamError::Unavailable)?;

        let discussion = &mut room.discussions[index];

        if discussion.state == DiscussionState::Completed {
            return Err(TeamError::Unavailable);
        }

        self.validate_mode(discussion.mode)?;

        discussion.pauses.retain(|reason| {
            !matches!(
                reason,
                PauseReason::User
                    | PauseReason::UserInput(_)
                    | PauseReason::ModeChange
                    | PauseReason::Reopened
                    | PauseReason::Closed
            )
        });

        if !discussion.pauses.is_empty() {
            return Err(TeamError::Paused);
        }

        cancel_pending_reservations(&mut room, id)?;

        let snapshot = room.public_snapshot();
        let discussion = &mut room.discussions[index];

        if let Some(stage) = discussion.stages.last_mut() {
            stage.segments.push(snapshot);
        }

        discussion.state = DiscussionState::Running;
        self.store.commit(room)?;

        Ok(())
    }

    pub fn add_turns(&mut self, id: DiscussionId, turns: u32) -> Result<(), TeamError> {
        let mut room = self.room().clone();

        let discussion = room
            .discussions
            .iter_mut()
            .find(|run| run.id == id)
            .ok_or(TeamError::Unavailable)?;

        discussion.budget.add_turns(turns)?;
        discussion.resolve_pause(&PauseReason::Budget);
        self.store.commit(room)?;

        Ok(())
    }

    pub fn change_mode(&mut self, id: DiscussionId, mode: DiscussionMode) -> Result<(), TeamError> {
        self.pause_discussion(id, PauseReason::ModeChange)?;

        if !self.slots.is_idle() {
            return Err(TeamError::Busy);
        }

        self.validate_mode(mode)?;

        let mut room = self.room().clone();

        cancel_pending_reservations(&mut room, id)?;

        let discussion = room
            .discussions
            .iter_mut()
            .find(|run| run.id == id)
            .ok_or(TeamError::Unavailable)?;

        discussion.change_mode(mode)?;

        for stage in &mut discussion.stages {
            if matches!(stage.kind, StageKind::ModeratorDecision | StageKind::Report) {
                for arrangement in &mut stage.arrangements {
                    if arrangement.state == ArrangementState::Pending {
                        arrangement.state = ArrangementState::Cancelled;
                    }
                }
            }
        }

        self.store.commit(room)?;

        Ok(())
    }

    pub fn skip_arrangement(
        &mut self,
        id: DiscussionId,
        operation: OperationId,
    ) -> Result<(), TeamError> {
        if !self.slots.is_idle() || !self.restored_uncertainty.is_empty() {
            return Err(TeamError::Unresolved);
        }

        let mut room = self.room().clone();

        cancel_pending_reservations(&mut room, id)?;

        let discussion = room
            .discussions
            .iter_mut()
            .find(|run| run.id == id)
            .ok_or(TeamError::Unavailable)?;

        let arrangement = discussion
            .stages
            .iter_mut()
            .flat_map(|stage| &mut stage.arrangements)
            .find(|entry| entry.operation == operation)
            .ok_or(TeamError::Unavailable)?;

        match arrangement.state {
            ArrangementState::Pending => {}

            ArrangementState::Failed(attempt) => {
                discussion
                    .pauses
                    .remove(&PauseReason::AttemptFailed(attempt));
            }

            _ => return Err(TeamError::Unresolved),
        }

        arrangement.state = ArrangementState::Skipped;
        discussion.pause(PauseReason::User);
        self.store.commit(room)?;

        Ok(())
    }

    pub fn finish_with_report(&mut self, id: DiscussionId) -> Result<(), TeamError> {
        self.pause_discussion(id, PauseReason::User)?;

        if !self.slots.is_idle() || !self.restored_uncertainty.is_empty() {
            return Err(TeamError::Unresolved);
        }

        let mut room = self.room().clone();

        cancel_pending_reservations(&mut room, id)?;

        let snapshot = room.public_snapshot();

        let discussion = room
            .discussions
            .iter_mut()
            .find(|run| run.id == id)
            .ok_or(TeamError::Unavailable)?;

        if discussion.state == DiscussionState::Completed {
            return Err(TeamError::Unavailable);
        }

        for arrangement in discussion
            .stages
            .iter_mut()
            .flat_map(|stage| &mut stage.arrangements)
        {
            if matches!(
                arrangement.state,
                ArrangementState::Pending | ArrangementState::Failed(_)
            ) {
                arrangement.state = ArrangementState::Cancelled;
            }
        }

        discussion.pauses.retain(|reason| {
            !matches!(
                reason,
                PauseReason::Budget | PauseReason::AttemptFailed(_) | PauseReason::User
            )
        });

        discussion.stages.push(Stage {
            decision: None,
            id: StageId::new(),
            kind: StageKind::Report,
            arrangements: vec![Arrangement {
                operation: OperationId::new(),
                recipient: discussion.mode.report_author(),
                state: ArrangementState::Pending,
            }],
            segments: vec![snapshot],
        });

        discussion.state = if discussion.pauses.is_empty() {
            DiscussionState::Finishing
        } else {
            DiscussionState::Paused
        };

        self.store.commit(room)?;

        Ok(())
    }
}

fn cancel_pending_reservations(room: &mut Room, id: DiscussionId) -> Result<(), TeamError> {
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
