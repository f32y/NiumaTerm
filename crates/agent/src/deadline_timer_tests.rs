use std::sync::mpsc::{RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use crate::deadline_timer::DeadlineTimer;

#[test]
fn idle_timer_rearms_for_earlier_work_and_stops_with_its_owner() {
    let (tx, rx) = channel();

    let timer = DeadlineTimer::new(move || {
        let _ = tx.send(());
    })
    .unwrap();

    assert_eq!(
        rx.recv_timeout(Duration::from_millis(20)),
        Err(RecvTimeoutError::Timeout)
    );

    timer.set(Some(Instant::now() + Duration::from_secs(30)));
    timer.set(Some(Instant::now()));
    rx.recv_timeout(Duration::from_secs(2)).unwrap();

    assert_eq!(
        rx.recv_timeout(Duration::from_millis(20)),
        Err(RecvTimeoutError::Timeout)
    );

    timer.set(Some(Instant::now() + Duration::from_secs(30)));
    timer.set(None);

    assert_eq!(
        rx.recv_timeout(Duration::from_millis(20)),
        Err(RecvTimeoutError::Timeout)
    );

    drop(timer);

    assert_eq!(
        rx.recv_timeout(Duration::from_secs(2)),
        Err(RecvTimeoutError::Disconnected)
    );
}
