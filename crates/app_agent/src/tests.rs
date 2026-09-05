use crate::GitBranchPoll;

#[test]
fn refresh_state_coalesces_requests_and_updates_presentation() {
    let mut poll = GitBranchPoll::default();
    assert_eq!(poll.presentation(), ("Detecting branch…".into(), 0.48));

    assert!(poll.begin_refresh());
    assert!(!poll.begin_refresh());

    poll.complete(Some("main".into()));
    assert_eq!(poll.presentation(), ("main".into(), 0.72));

    assert!(poll.begin_refresh());
    poll.complete(None);
    assert_eq!(poll.presentation(), ("No Git branch".into(), 0.48));
}
