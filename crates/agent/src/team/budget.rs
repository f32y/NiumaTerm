use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::team::identity::AttemptId;

pub const DEFAULT_TURN_LIMIT: u32 = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnPurpose {
    Response,
    Moderation,
    Summary,
    Retry,
    Report,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReservationState {
    Unsent,
    Charged,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reservation {
    pub purpose: TurnPurpose,
    pub state: ReservationState,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Budget {
    limit: u32,
    report_reserved: bool,
    reservations: BTreeMap<AttemptId, Reservation>,
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum BudgetError {
    #[error("add scheduled turns or finish with a report")]
    InsufficientTurns,

    #[error("attempt already has a budget reservation")]
    DuplicateAttempt,

    #[error("attempt has no budget reservation")]
    MissingAttempt,

    #[error("a dispatched or uncertain attempt keeps its charge")]
    AlreadyDispatched,

    #[error("scheduled turn limit is too large")]
    Overflow,
}

impl Budget {
    pub fn discussion() -> Self {
        Self {
            limit: DEFAULT_TURN_LIMIT,
            report_reserved: true,
            reservations: BTreeMap::new(),
        }
    }

    pub fn direct() -> Self {
        Self {
            report_reserved: false,
            ..Self::discussion()
        }
    }

    pub fn limit(&self) -> u32 {
        self.limit
    }

    pub(crate) fn reservations(&self) -> &BTreeMap<AttemptId, Reservation> {
        &self.reservations
    }

    pub fn remaining_non_report_turns(&self) -> u32 {
        let report = self.report_reserved
            && !self.reservations.values().any(|entry| {
                entry.purpose == TurnPurpose::Report && entry.state == ReservationState::Unsent
            });

        self.limit
            .saturating_sub(u32::try_from(self.reservations.len()).unwrap_or(u32::MAX))
            .saturating_sub(u32::from(report))
    }

    pub(super) fn validate(&self) -> bool {
        let needs_report = self.report_reserved
            && !self
                .reservations
                .values()
                .any(|entry| entry.purpose == TurnPurpose::Report);

        self.reservations.len() as u64 + u64::from(needs_report) <= u64::from(self.limit)
    }

    pub fn add_turns(&mut self, turns: u32) -> Result<(), BudgetError> {
        self.limit = self.limit.checked_add(turns).ok_or(BudgetError::Overflow)?;

        Ok(())
    }

    /// Validate the entire batch before inserting anything. A stage must not
    /// silently lose its later recipients when its allowance cannot fit them.
    pub fn reserve(&mut self, batch: &[(AttemptId, TurnPurpose)]) -> Result<(), BudgetError> {
        let mut next = self.reservations.clone();

        for &(id, purpose) in batch {
            if next
                .insert(
                    id,
                    Reservation {
                        purpose,
                        state: ReservationState::Unsent,
                    },
                )
                .is_some()
            {
                return Err(BudgetError::DuplicateAttempt);
            }
        }

        let needs_report = self.report_reserved
            && !next.values().any(|reservation| {
                reservation.purpose == TurnPurpose::Report
                    && reservation.state == ReservationState::Unsent
            });

        if next.len() as u64 + u64::from(needs_report) > u64::from(self.limit) {
            return Err(BudgetError::InsufficientTurns);
        }

        self.reservations = next;

        Ok(())
    }

    /// Charge before the external send. Unknown acceptance has the same cost
    /// as accepted work until reliable non-delivery evidence is available.
    pub fn charge(&mut self, id: AttemptId) -> Result<(), BudgetError> {
        self.reservations
            .get_mut(&id)
            .ok_or(BudgetError::MissingAttempt)?
            .state = ReservationState::Charged;

        Ok(())
    }

    pub(crate) fn cancel_unsent(&mut self, id: AttemptId) -> Result<(), BudgetError> {
        match self
            .reservations
            .get(&id)
            .ok_or(BudgetError::MissingAttempt)?
            .state
        {
            ReservationState::Unsent => {
                self.reservations.remove(&id);

                Ok(())
            }

            ReservationState::Charged => Err(BudgetError::AlreadyDispatched),
        }
    }

    pub(super) fn release_rejected(&mut self, id: AttemptId) -> Result<(), BudgetError> {
        self.reservations
            .remove(&id)
            .ok_or(BudgetError::MissingAttempt)?;

        Ok(())
    }
}
