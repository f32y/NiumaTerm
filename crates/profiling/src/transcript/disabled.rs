//! Transcript hooks with no clocks, counters, or reporting task.

#[path = "operation.rs"]
mod operation;

pub use crate::transcript::operation::Operation;

/// No measurement state is carried by ordinary builds.
#[must_use]
pub enum Probe {}

impl Probe {
    /// Keep update call sites identical without opening a measurement scope.
    #[inline(always)]
    pub fn start(_: Operation) -> Option<Self> {
        None
    }
}

/// No samples are retained without performance collection.
#[inline(always)]
pub fn flush() {}

#[cfg(test)]
#[path = "disabled_tests.rs"]
mod tests;
