use crate::GitBranchPoll;

#[test]
fn refresh_state_coalesces_requests_and_updates_presentation() {
    let mut poll = GitBranchPoll::default();
    assert_eq!(poll.presentation(), ("Detecting branch…".into(), 0.48));

    let generation = poll.begin_refresh().unwrap();
    assert!(poll.begin_refresh().is_none());

    poll.complete(generation, Some("main".into()));
    assert_eq!(poll.presentation(), ("main".into(), 0.72));

    let generation = poll.begin_refresh().unwrap();
    poll.complete(generation, None);
    assert_eq!(poll.presentation(), ("No Git branch".into(), 0.48));
}

#[test]
fn changing_directory_discards_in_flight_branch_results() {
    let mut poll = GitBranchPoll::default();
    let initial = poll.begin_refresh().unwrap();
    poll.complete(initial, Some("main".into()));
    let old = poll.begin_refresh().unwrap();

    poll.invalidate();
    assert_eq!(poll.presentation(), ("Detecting branch…".into(), 0.48));
    let current = poll.begin_refresh().unwrap();
    poll.complete(old, Some("main".into()));
    assert_eq!(poll.presentation(), ("Detecting branch…".into(), 0.48));
    assert!(poll.begin_refresh().is_none());

    poll.complete(current, Some("feature/uv-editor".into()));
    poll.complete(old, Some("main".into()));
    assert_eq!(poll.presentation(), ("feature/uv-editor".into(), 0.72));
}
