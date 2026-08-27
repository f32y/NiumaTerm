use std::sync::Arc;

use parking_lot::Mutex;

use crate::wake::{Wake, WakeSender, wake_channel};

#[test]
fn callback_wake_sender_forwards_wake() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen_for_sender = Arc::clone(&seen);
    let sender = WakeSender::from_fn(move |wake| {
        seen_for_sender.lock().push(wake);
    });

    sender.send(Wake::Content(7));

    assert_eq!(*seen.lock(), vec![Wake::Content(7)]);
}

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
