use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::team::attempt::{Attempt, AttemptState};

const DEFAULT_TURN_LIMIT: u32 = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnPurpose {
    Response,
    Moderation,
    Summary,
    Report,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Budget {
    limit: u32,
    report_reserved: bool,
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum BudgetError {
    #[error("add scheduled turns or finish with a report")]
    InsufficientTurns,
    #[error("scheduled turn limit is too large")]
    Overflow,
}

impl Budget {
    pub(super) fn discussion() -> Self {
        Self {
            limit: DEFAULT_TURN_LIMIT,
            report_reserved: true,
        }
    }

    pub(super) fn direct() -> Self {
        Self {
            report_reserved: false,
            ..Self::discussion()
        }
    }

    pub(super) fn remaining_non_report_turns<'a>(
        &self,
        attempts: impl Iterator<Item = &'a Attempt>,
    ) -> u32 {
        let mut used = 0u32;
        let mut needs_report = self.report_reserved;

        for attempt in attempts {
            used = used.saturating_add(1);

            if attempt.intent.purpose == TurnPurpose::Report
                && attempt.state == AttemptState::Reserved
            {
                needs_report = false;
            }
        }

        self.limit
            .saturating_sub(used)
            .saturating_sub(u32::from(needs_report))
    }

    pub(super) fn validate<'a>(&self, attempts: impl Iterator<Item = &'a Attempt>) -> bool {
        let mut used = 0u64;
        let mut needs_report = self.report_reserved;

        for attempt in attempts {
            used += 1;

            if attempt.intent.purpose == TurnPurpose::Report {
                needs_report = false;
            }
        }

        used + u64::from(needs_report) <= u64::from(self.limit)
    }

    /// Reserve space for the entire batch before storing any attempt. Unknown
    /// delivery and failed dispatched work still consume their scheduled turns.
    pub(super) fn check_batch<'a>(
        &self,
        attempts: impl Iterator<Item = &'a Attempt>,
        purposes: impl Iterator<Item = TurnPurpose>,
    ) -> Result<(), BudgetError> {
        let mut used = 0u64;
        let mut needs_report = self.report_reserved;

        for (purpose, unsent) in attempts
            .map(|attempt| {
                (
                    attempt.intent.purpose,
                    attempt.state == AttemptState::Reserved,
                )
            })
            .chain(purposes.map(|purpose| (purpose, true)))
        {
            used += 1;

            if purpose == TurnPurpose::Report && unsent {
                needs_report = false;
            }
        }

        if used + u64::from(needs_report) > u64::from(self.limit) {
            return Err(BudgetError::InsufficientTurns);
        }

        Ok(())
    }

    pub(super) fn add_turns(&mut self, turns: u32) -> Result<(), BudgetError> {
        self.limit = self.limit.checked_add(turns).ok_or(BudgetError::Overflow)?;

        Ok(())
    }
}
