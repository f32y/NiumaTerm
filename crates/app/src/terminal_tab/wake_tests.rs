use crate::terminal_tab::wake::{Wake, wake_channel};

#[test]
fn coalesces_until_delivered() {
    let (wake, mut rx) = wake_channel();

    assert!(wake.signal(Wake::Content(7)));
    assert!(!wake.signal(Wake::Content(7)));
    assert!(rx.try_recv().is_ok());
    assert!(rx.try_recv().is_err());

    wake.mark_delivered(7);

    assert!(rx.try_recv().is_ok());
    assert!(rx.try_recv().is_err());

    wake.mark_delivered(7);

    assert!(wake.signal(Wake::Content(7)));
    assert!(rx.try_recv().is_ok());
}

#[test]
fn inactive_content_cannot_suppress_chrome() {
    let (wake, mut rx) = wake_channel();

    assert!(wake.signal(Wake::Content(7)));
    assert_eq!(rx.try_recv(), Ok(Wake::Content(7)));
    assert!(!wake.signal(Wake::Content(7)));
    assert!(rx.try_recv().is_err());
    assert!(wake.signal(Wake::Chrome(7)));
    assert_eq!(rx.try_recv(), Ok(Wake::Chrome(7)));
}
