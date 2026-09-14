//! A turn from the moment work starts to the moment it settles, and the
//! prompts the agent raises inside one.
//!
//! The transcript reads how long the conversation has been waiting from when
//! the last answer settled, so the moments recorded here are what the idle
//! reading is built on.

use crate::agent_tab::transcript::LAST_RESPONSE_LIMIT;
use std::time::Duration;

pub(crate) fn response_age_tick(age: Duration) -> Option<Duration> {
    const MINUTE: u64 = 60;

    match age.as_secs() {
        ..MINUTE => Some(Duration::from_secs(1)),
        seconds if seconds < LAST_RESPONSE_LIMIT.as_secs() => Some(Duration::from_secs(MINUTE)),
        _ => None,
    }
}
