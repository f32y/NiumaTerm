use crate::team::attempt::{Attempt, AttemptState};
use crate::team::discussion::{ArrangementState, PauseReason};
use crate::team::identity::AttemptId;
use crate::team::session::{TeamError, TeamSession};

impl TeamSession {
    pub fn pending_recovery(&self) -> impl Iterator<Item = &Attempt> {
        self.room()
            .attempts()
            .iter()
            .filter(|attempt| self.restored_uncertainty.contains(&attempt.id))
    }

    /// The user can end tracking restored work without claiming that the
    /// provider never ran it. Its consumed budget and provider identity remain.
    pub fn abandon_restored_attempt(&mut self, id: AttemptId) -> Result<(), TeamError> {
        if !self.slots.is_idle() || !self.restored_uncertainty.contains(&id) {
            return Err(TeamError::Unresolved);
        }

        let mut room = self.room().clone();

        let attempt = room
            .attempts
            .iter_mut()
            .find(|attempt| attempt.id == id)
            .ok_or(TeamError::Unavailable)?;

        attempt.state = AttemptState::Abandoned;

        for discussion in &mut room.discussions {
            for arrangement in discussion
                .stages
                .iter_mut()
                .flat_map(|stage| &mut stage.arrangements)
            {
                if matches!(arrangement.state, ArrangementState::Active(attempt) | ArrangementState::Uncertain(attempt) if attempt == id)
                {
                    arrangement.state = ArrangementState::Skipped;
                }
            }

            discussion.resolve_pause(&PauseReason::UncertainAttempt(id));
            discussion.pause(PauseReason::User);
            discussion.settle_pause();
        }

        self.commit_room(room)?;
        self.restored_uncertainty.remove(&id);

        Ok(())
    }
}
