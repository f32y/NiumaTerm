use std::mem::size_of;

use crate::transcript::{Operation, Probe, flush};

#[test]
fn runtime_switch_cannot_create_transcript_samples() {
    crate::set_enabled(true);

    assert!(!crate::enabled());
    assert!(Probe::start(Operation::AppendDelta).is_none());
    assert_eq!(size_of::<Option<Probe>>(), 0);

    flush();
}
