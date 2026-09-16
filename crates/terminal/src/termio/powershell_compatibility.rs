#[cfg(test)]
mod tests;

use std::time::{Duration, Instant};

pub(super) const RESIZE_INPUT_DELAY: Duration = Duration::from_millis(80);
const MAX_QUEUED_INPUT_DELAY: Duration = Duration::from_millis(250);

/// A short pause after native resize lets PowerShell process pending console
/// changes. Elapsed time is a compatibility heuristic, not a readiness signal.
#[derive(Default)]
pub(super) struct PowerShellCompatibility {
    enabled: bool,
    resize_deadline: Option<Instant>,
}

impl PowerShellCompatibility {
    pub(super) fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;

        if !enabled {
            self.resize_deadline = None;
        }
    }

    pub(super) fn resized(&mut self, now: Instant) {
        self.resize_deadline = self.enabled.then_some(now + RESIZE_INPUT_DELAY);
    }

    pub(super) fn clear_resize(&mut self) {
        self.resize_deadline = None;
    }

    pub(super) fn input_limit(&self, now: Instant) -> Option<Instant> {
        self.enabled.then_some(now + MAX_QUEUED_INPUT_DELAY)
    }

    pub(super) fn timeout(&self, input_limit: Option<Instant>, now: Instant) -> Option<Duration> {
        // Interleaved input/resize bursts retain submission order. An old input
        // must eventually bypass later resize pauses instead of accumulating
        // another 80 ms for every size change ahead of it.
        let deadline = self.resize_deadline?.min(input_limit?);

        // An expired deadline still wakes poll until the queued input executes;
        // returning None here could park a quiet session indefinitely.
        Some(deadline.saturating_duration_since(now))
    }
}
