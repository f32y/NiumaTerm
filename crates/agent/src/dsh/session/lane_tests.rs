use std::sync::mpsc;
use std::time::{Duration, Instant};

use tokio::time::sleep;

use crate::dsh::session::lane::CommandLane;

#[test]
fn commands_return_at_once_and_run_in_the_order_they_were_given() {
    let lane = CommandLane::new();
    let (done_tx, done) = mpsc::channel();

    let asked = Instant::now();

    for (name, delay) in [("slow", 200), ("fast", 0)] {
        let done_tx = done_tx.clone();

        lane.run(async move {
            sleep(Duration::from_millis(delay)).await;

            let _ = done_tx.send(name);
        });
    }

    assert!(asked.elapsed() < Duration::from_millis(100));

    let order: Vec<_> = (0..2)
        .map(|_| done.recv_timeout(Duration::from_secs(2)).unwrap())
        .collect();

    assert_eq!(order, ["slow", "fast"]);
}

#[test]
fn a_dropped_lane_runs_nothing_further() {
    let lane = CommandLane::new();
    let (done_tx, done) = mpsc::channel();

    for delay in [200, 0] {
        let done_tx = done_tx.clone();

        lane.run(async move {
            sleep(Duration::from_millis(delay)).await;

            let _ = done_tx.send(());
        });
    }

    drop(lane);

    drop(done_tx);

    assert!(done.recv_timeout(Duration::from_secs(1)).is_err());
}
