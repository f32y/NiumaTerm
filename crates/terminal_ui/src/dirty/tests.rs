use crate::dirty::DirtyState;

#[test]
fn dirty_state_coalesces_until_frame_begins() {
    let mut dirty = DirtyState::default();

    assert!(dirty.mark());
    assert!(!dirty.mark());
    assert!(dirty.is_pending());

    assert!(dirty.begin_frame());
    assert!(!dirty.is_pending());
    assert!(!dirty.begin_frame());
}
