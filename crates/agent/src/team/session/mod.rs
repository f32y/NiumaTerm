//! Durable room operations and live dispatch readiness owned by one Team.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use thiserror::Error;

use crate::chat::SendOutcome;
use crate::session::team_capabilities::TeamCapabilities;
use crate::team::attempt::{AttemptState, BudgetScope, DispatchIntent};
use crate::team::budget::BudgetError;
use crate::team::context::ContextError;
use crate::team::discussion::{ArrangementState, DiscussionError, DiscussionState, PauseReason};
use crate::team::execution_slots::{ExecutionKey, ExecutionSlots, WorkStatus};
use crate::team::identity::{AttemptId, MemberId, OwnershipGeneration, RoomId};
use crate::team::room::{MemberError, Room};
use crate::team::storage::{DispatchError, RecoveryNotice, RoomStore, StorageError};

pub struct TeamSession {
    store: RoomStore,
    readiness: BTreeMap<MemberId, MemberReadiness>,
    slots: ExecutionSlots,
    restored_uncertainty: BTreeSet<AttemptId>,
}

mod controls;
mod members;
mod recovery;
mod summaries;

pub use crate::team::session::summaries::{SummaryRequest, SummaryText};

mod outcomes;
mod planning;
#[cfg(test)]
mod planning_tests;
#[cfg(test)]
mod tests;

pub use crate::team::session::outcomes::AttemptEventKey;

struct MemberReadiness {
    ownership: OwnershipGeneration,
    backend_generation: u64,
    capabilities: TeamCapabilities,
}

#[derive(Debug, Error)]
pub enum TeamError {
    #[error(transparent)]
    Member(#[from] MemberError),

    #[error(transparent)]
    Storage(#[from] StorageError),

    #[error(transparent)]
    Dispatch(#[from] DispatchError),

    #[error("member is unavailable")]
    Unavailable,

    #[error("discussion is paused")]
    Paused,

    #[error("member is busy")]
    Busy,

    #[error(transparent)]
    Discussion(#[from] DiscussionError),

    #[error(transparent)]
    Context(#[from] ContextError),

    #[error(transparent)]
    Budget(#[from] BudgetError),

    #[error("this arrangement cannot be changed while its work is unresolved")]
    Unresolved,
}

impl TeamSession {
    pub(in crate::team) fn commit_room(&mut self, room: Room) -> Result<(), TeamError> {
        self.store.commit(room)?;

        Ok(())
    }

    pub fn create(data_directory: &Path, room: Room) -> Result<Self, TeamError> {
        Ok(Self {
            store: RoomStore::create(data_directory, room)?,
            readiness: BTreeMap::new(),
            slots: ExecutionSlots::default(),
            restored_uncertainty: BTreeSet::new(),
        })
    }

    pub fn open(
        data_directory: &Path,
        id: RoomId,
    ) -> Result<(Self, Vec<RecoveryNotice>), TeamError> {
        let (mut store, notices) = RoomStore::open(data_directory, id)?;
        let mut room = store.room().clone();

        for discussion in &mut room.discussions {
            discussion.pause(PauseReason::Reopened);
        }

        for attempt in &mut room.attempts {
            if matches!(
                attempt.state,
                AttemptState::Sending | AttemptState::Accepted { .. }
            ) {
                attempt.state = AttemptState::Uncertain;
            }
        }

        let restored_uncertainty = room
            .attempts
            .iter()
            .filter(|attempt| {
                matches!(
                    attempt.state,
                    AttemptState::Sending | AttemptState::Accepted { .. } | AttemptState::Uncertain
                )
            })
            .map(|attempt| attempt.id)
            .collect();

        for discussion in &mut room.discussions {
            for stage in &mut discussion.stages {
                for arrangement in &mut stage.arrangements {
                    if let ArrangementState::Active(id) = arrangement.state {
                        arrangement.state = ArrangementState::Uncertain(id);
                    }
                }
            }
        }

        store.commit(room)?;

        Ok((
            Self {
                store,
                readiness: BTreeMap::new(),
                slots: ExecutionSlots::default(),
                restored_uncertainty,
            },
            notices,
        ))
    }

    pub fn room(&self) -> &Room {
        self.store.room()
    }

    pub fn revision(&self) -> u64 {
        self.store.revision()
    }

    pub fn member_ready(
        &mut self,
        id: MemberId,
        backend_generation: u64,
        capabilities: TeamCapabilities,
    ) -> Result<(), TeamError> {
        let member = self.room().member(id).ok_or(TeamError::Unavailable)?;

        if member.excluded() {
            return Err(TeamError::Unavailable);
        }

        let ownership = member.ownership();
        let mut room = self.room().clone();
        let mut changed = false;

        for discussion in &mut room.discussions {
            changed |= discussion.resolve_pause(&PauseReason::MemberUnavailable(id));
        }

        if changed {
            self.store.commit(room)?;
        }

        self.readiness.insert(
            id,
            MemberReadiness {
                ownership,
                backend_generation,
                capabilities,
            },
        );

        Ok(())
    }

    pub fn reserve_dispatches(
        &mut self,
        intents: Vec<DispatchIntent>,
    ) -> Result<Vec<AttemptId>, TeamError> {
        for intent in &intents {
            self.validate_recipient(intent)?;

            for attachment in intent.input.attachments.iter().chain(&intent.attachments) {
                self.store.read_attachment(attachment)?;
            }
        }

        Ok(self.store.reserve_dispatches(intents)?)
    }

    pub fn dispatch(
        &mut self,
        id: AttemptId,
        send: impl FnOnce(&DispatchIntent) -> SendOutcome,
    ) -> Result<SendOutcome, TeamError> {
        let attempt = self
            .room()
            .attempts()
            .iter()
            .find(|attempt| attempt.id == id)
            .ok_or(TeamError::Unavailable)?;

        self.validate_recipient(&attempt.intent)?;

        if let BudgetScope::Discussion(id) = attempt.intent.budget {
            let run = self
                .room()
                .discussions()
                .iter()
                .find(|run| run.id() == id)
                .ok_or(TeamError::Unavailable)?;

            if !matches!(
                run.state(),
                DiscussionState::Running | DiscussionState::Finishing
            ) || !run.pauses().is_empty()
            {
                return Err(TeamError::Paused);
            }
        }

        let key = ExecutionKey {
            member: attempt.intent.recipient,
            ownership: attempt.intent.ownership,
            attempt: id,
        };

        if !self.slots.reserve(key) {
            return Err(TeamError::Busy);
        }

        let mut sent = false;

        let result = self.store.dispatch(id, |intent| {
            sent = true;

            send(intent)
        });

        match &result {
            Ok(SendOutcome::NotReady) => {
                self.slots.update(key, WorkStatus::default());
            }

            Ok(SendOutcome::StartedTurn) => {}

            Ok(SendOutcome::Steered | SendOutcome::Rejected { .. }) => {
                self.slots.update(
                    key,
                    WorkStatus {
                        uncertain: true,
                        ..WorkStatus::default()
                    },
                );
            }

            Err(_) if !sent => {
                self.slots.update(key, WorkStatus::default());
            }

            Err(_) => {
                self.slots.update(
                    key,
                    WorkStatus {
                        uncertain: true,
                        ..WorkStatus::default()
                    },
                );
            }
        }

        Ok(result?)
    }

    pub fn update_work(&mut self, key: ExecutionKey, status: WorkStatus) -> bool {
        self.slots.update(key, status)
    }

    pub fn member_unavailable(&mut self, member: MemberId) -> Result<(), TeamError> {
        self.readiness.remove(&member);

        let mut room = self.room().clone();

        for discussion in &mut room.discussions {
            discussion.pause(PauseReason::MemberUnavailable(member));
        }

        self.store.commit(room)?;

        Ok(())
    }

    fn validate_recipient(&self, intent: &DispatchIntent) -> Result<(), TeamError> {
        let readiness = self
            .readiness
            .get(&intent.recipient)
            .ok_or(TeamError::Unavailable)?;

        if readiness.ownership != intent.ownership
            || readiness.backend_generation != intent.backend_generation
        {
            return Err(TeamError::Unavailable);
        }

        self.validate_member(intent.recipient)
    }

    pub(in crate::team) fn validate_member(&self, id: MemberId) -> Result<(), TeamError> {
        if !self.restored_uncertainty.is_empty() {
            return Err(TeamError::Unavailable);
        }

        let member = self.room().member(id).ok_or(TeamError::Unavailable)?;
        let readiness = self.readiness.get(&id).ok_or(TeamError::Unavailable)?;

        if member.excluded() || member.ownership() != readiness.ownership {
            return Err(TeamError::Unavailable);
        }

        Ok(())
    }
}
