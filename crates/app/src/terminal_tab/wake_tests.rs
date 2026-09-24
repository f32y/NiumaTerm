use nmt_terminal::session::SessionChange;

use crate::terminal_tab::wake::wake_channel;

#[test]
fn coalesces_until_delivered() {
    let (wake, mut rx) = wake_channel();

    assert!(wake.signal(SessionChange::Content));
    assert!(!wake.signal(SessionChange::Content));
    assert!(rx.try_recv().is_ok());
    assert!(rx.try_recv().is_err());

    wake.mark_delivered();

    assert!(rx.try_recv().is_ok());
    assert!(rx.try_recv().is_err());

    wake.mark_delivered();

    assert!(wake.signal(SessionChange::Content));
    assert!(rx.try_recv().is_ok());
}

#[test]
fn inactive_content_cannot_suppress_chrome() {
    let (wake, mut rx) = wake_channel();

    assert!(wake.signal(SessionChange::Content));
    assert_eq!(rx.try_recv(), Ok(SessionChange::Content));
    assert!(!wake.signal(SessionChange::Content));
    assert!(rx.try_recv().is_err());
    assert!(wake.signal(SessionChange::HostEvents));
    assert_eq!(rx.try_recv(), Ok(SessionChange::HostEvents));
}
