use std::io::{Read, Write};
use std::thread::sleep;
use std::time::Duration;

use miow::pipe::anonymous;

use crate::windows::pipes::*;

/// Spin until `cond` holds, up to ~2s, so the worker thread has time to move
/// pipe bytes into the ring. Returns whether it held within the budget.
fn wait_until(mut cond: impl FnMut() -> bool) -> bool {
    for _ in 0..2000 {
        if cond() {
            return true;
        }
        sleep(Duration::from_millis(1));
    }
    cond()
}

/// Regression: a consumer that stops reading with data still buffered must leave
/// the soft-ready flag set, so the event loop's `has_ready()` re-observes it instead
/// of sleeping on already-signalled data (the vtebench output-freeze bug). The flag
/// only clears once the ring is fully drained.
#[test]
fn soft_ready_stays_set_until_ring_fully_drained() {
    let (conout, mut pty_side) = anonymous(0).expect("anonymous pipe");
    let mut reader = EventedAnonRead::new(conout);

    // Push more than a single small read will drain.
    pty_side.write_all(&[0xABu8; 4096]).expect("write");

    // Worker moves the bytes into the ring and arms the flag.
    assert!(
        wait_until(|| reader.soft().is_ready()),
        "flag should arm once data lands in the ring"
    );

    // Read only a slice — data remains buffered.
    let mut small = [0u8; 16];
    let got = reader.read(&mut small).expect("partial read");
    assert!(got > 0 && got <= 16);
    assert!(
        reader.soft().is_ready(),
        "flag must stay set while the ring still holds data"
    );

    // Drain the rest; the flag clears only once the ring is empty.
    let mut drained = got;
    let mut sink = [0u8; 4096];
    while drained < 4096 {
        match reader.read(&mut sink) {
            Ok(0) => {
                // Ring momentarily empty but more may be in flight; let the worker run.
                if !wait_until(|| reader.soft().is_ready()) {
                    break;
                }
            }
            Ok(n) => drained += n,
            Err(e) => panic!("drain read failed: {e}"),
        }
    }
    assert_eq!(drained, 4096, "should read back every byte written");

    assert!(
        wait_until(|| !reader.soft().is_ready()),
        "flag must clear once the ring is fully drained"
    );
}
