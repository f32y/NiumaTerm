//! The turn currently running, and what each finished one is remembered by.
//!
//! Both are view state rather than provider transcript content, so neither
//! reaches the shared item stream. They are separate because they have
//! different lifetimes: the live values exist only while a turn is in flight
//! and are consumed when it settles, while the ledger keeps one entry per turn
//! for as long as the conversation is on screen.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

/// The running turn: when it started, what it has produced, and what it is
/// doing right now.
#[derive(Default)]
pub(crate) struct LiveTurn {
    /// When the running turn began, which is what the elapsed reading counts
    /// from. `None` between turns.
    started: Option<Instant>,
    /// Output tokens reported so far for the running turn.
    output_tokens: Option<u64>,
    /// What the backend last said it was doing, shown beside the elapsed
    /// reading.
    detail: Option<String>,
    /// The backend is compacting rather than answering, which the progress
    /// line names instead of the usual reading.
    compacting: bool,
}

impl LiveTurn {
    pub(crate) fn is_working(&self) -> bool {
        self.started.is_some()
    }

    pub(crate) fn started(&self) -> Option<Instant> {
        self.started
    }

    pub(crate) fn output_tokens(&self) -> Option<u64> {
        self.output_tokens
    }

    pub(crate) fn detail(&self) -> Option<&str> {
        self.detail.as_deref()
    }

    pub(crate) fn is_compacting(&self) -> bool {
        self.compacting
    }

    pub(crate) fn set_compacting(&mut self, compacting: bool) {
        self.compacting = compacting;
    }

    pub(crate) fn start(&mut self) {
        self.started = Some(Instant::now());
        self.output_tokens = None;
        // Whatever the last turn was doing has nothing to say about this one.
        self.detail = None;
    }

    /// Report what the running turn is doing, or clear it once the backend has
    /// nothing further to add. Returns whether that is a change.
    pub(crate) fn set_detail(&mut self, detail: Option<String>) -> bool {
        if self.detail == detail {
            return false;
        }

        self.detail = detail;

        true
    }

    /// Record output usage, reporting whether a turn was running to record it
    /// against. Counts arriving between turns describe the one that just
    /// ended, which already has its own total.
    pub(crate) fn set_output_tokens(&mut self, output_tokens: u64) -> bool {
        if self.started.is_none() {
            return false;
        }

        self.output_tokens = Some(output_tokens);

        true
    }

    /// End the running turn, handing back what the ledger needs. The detail
    /// goes either way: a turn that never started still has nothing left to
    /// report.
    pub(crate) fn finish(&mut self) -> Option<(Instant, Option<u64>)> {
        let output_tokens = self.output_tokens.take();

        self.detail = None;

        self.started.take().map(|started| (started, output_tokens))
    }

    /// Drop the running turn without settling it.
    pub(crate) fn discard(&mut self) {
        self.started = None;
        self.output_tokens = None;
        self.detail = None;
        self.compacting = false;
    }
}

/// One entry per finished turn: whether it settled, how long it took, what it
/// produced, and whether the user stopped it.
#[derive(Default)]
pub(crate) struct TurnLedger {
    /// Turns that have finished, whether in this process or in a session this
    /// view replayed. Folding keys off this rather than off a known duration,
    /// because a replayed turn has no duration to record.
    settled: HashSet<u64>,
    seconds: HashMap<u64, u64>,
    output_tokens: HashMap<u64, u64>,
    /// Turns the user stopped. An interrupted turn reports no elapsed time,
    /// because the reading would describe how long the user waited before
    /// giving up rather than how long the work took.
    interrupted: HashSet<u64>,
}

impl TurnLedger {
    pub(crate) fn is_settled(&self, turn: u64) -> bool {
        self.settled.contains(&turn)
    }

    pub(crate) fn was_interrupted(&self, turn: u64) -> bool {
        self.interrupted.contains(&turn)
    }

    pub(crate) fn mark_interrupted(&mut self, turn: u64) {
        self.interrupted.insert(turn);
    }

    pub(crate) fn seconds(&self, turn: u64) -> Option<u64> {
        self.seconds.get(&turn).copied()
    }

    pub(crate) fn output_tokens(&self, turn: u64) -> Option<u64> {
        self.output_tokens.get(&turn).copied()
    }

    /// Close a turn that ran in this process.
    pub(crate) fn settle(&mut self, turn: u64, seconds: u64, output_tokens: Option<u64>) {
        self.settled.insert(turn);

        if !self.interrupted.contains(&turn) {
            self.seconds.insert(turn, seconds);
        }

        if let Some(output_tokens) = output_tokens {
            self.output_tokens.insert(turn, output_tokens);
        }
    }

    /// Close a turn replayed from a stored conversation, with the accounting
    /// the provider persisted for it.
    pub(crate) fn replay(
        &mut self,
        turn: u64,
        interrupted: bool,
        seconds: Option<u64>,
        output_tokens: Option<u64>,
    ) {
        self.settled.insert(turn);

        if interrupted {
            self.interrupted.insert(turn);
        }

        if let Some(seconds) = seconds {
            self.seconds.insert(turn, seconds);
        }

        if let Some(output_tokens) = output_tokens {
            self.output_tokens.insert(turn, output_tokens);
        }
    }

    /// Forget a turn that never produced visible output, so an immediate stop
    /// leaves no elapsed-time row behind for work that did not happen.
    pub(crate) fn forget(&mut self, turn: u64) {
        self.settled.remove(&turn);
        self.seconds.remove(&turn);
        self.output_tokens.remove(&turn);
    }

    pub(crate) fn clear(&mut self) {
        self.settled.clear();
        self.seconds.clear();
        self.output_tokens.clear();
        self.interrupted.clear();
    }
}

#[cfg(test)]
impl TurnLedger {
    /// Close a turn as a stored conversation would, for tests that need a
    /// settled turn without running one.
    pub(crate) fn settle_replayed(&mut self, turn: u64, seconds: Option<u64>) {
        self.replay(turn, false, seconds, None);
    }
}
