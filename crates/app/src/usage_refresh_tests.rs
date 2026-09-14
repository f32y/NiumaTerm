use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::usage_refresh::{Completion, FetchError, Refresh, UsageSource};

#[test]
fn disabling_and_reenabling_waits_for_cancelled_work_then_retries() {
    let calls = Arc::new(AtomicUsize::new(0));

    let source: UsageSource<usize> = Arc::new({
        let calls = calls.clone();

        move |_: &AtomicBool| Ok(calls.fetch_add(1, Ordering::Relaxed) + 1)
    });

    let mut refresh = Refresh::new(0, source, true);

    let first = refresh.begin().unwrap().run();

    refresh.set_enabled(false);

    refresh.set_enabled(true);

    assert!(refresh.begin().is_none());
    assert!(matches!(refresh.complete(first), Completion::Retry));
    assert_eq!(refresh.value, 0);

    let second = refresh.begin().unwrap().run();

    assert!(matches!(refresh.complete(second), Completion::Updated));
    assert_eq!(refresh.value, 2);
    assert!(!refresh.refreshing());
}

#[test]
fn failed_refresh_retains_the_last_value_until_a_later_success() {
    let calls = AtomicUsize::new(0);

    let source = Arc::new(move |_: &AtomicBool| {
        if calls.fetch_add(1, Ordering::Relaxed) == 0 {
            Err(FetchError::Failed("unavailable".into()))
        } else {
            Ok(9)
        }
    });

    let mut refresh = Refresh::new(7, source, true);

    let failed = refresh.begin().unwrap().run();

    assert!(
        matches!(refresh.complete(failed), Completion::Failed(message) if message == "unavailable")
    );
    assert_eq!(refresh.value, 7);
    assert!(refresh.failed);

    let recovered = refresh.begin().unwrap().run();

    assert!(matches!(refresh.complete(recovered), Completion::Updated));
    assert_eq!(refresh.value, 9);
    assert!(!refresh.failed);
}

#[test]
fn dropping_refresh_cancels_queued_work_without_starting_the_source() {
    let mut refresh = Refresh::new(
        0,
        Arc::new(|_: &AtomicBool| -> Result<i32, FetchError> {
            panic!("cancelled source must not start")
        }),
        true,
    );

    let fetch = refresh.begin().unwrap();

    drop(refresh);

    let _ = fetch.run();
}
