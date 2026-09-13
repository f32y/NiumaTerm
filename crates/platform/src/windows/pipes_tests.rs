use std::io::{Read, Write};
use std::thread::sleep;
use std::time::Duration;

use miow::pipe::anonymous;

use crate::windows::pipes::*;

#[test]
fn a_pipe_error_after_its_consumer_closes_does_not_panic() {
    let (reader, writer) = anonymous(0).unwrap();
    let (producer, _consumer) = spsc_buffer(16);
    let (error_sender, error_receiver) = channel();

    let inner = Arc::new(PipeState {
        soft: SoftReady::new(),
        done: AtomicBool::new(false),
        buffer_changed: Condvar::new(),
        wait_tag: Mutex::new(()),
    });

    drop(error_receiver);
    drop(writer);

    assert!(
        spawn(move || pump_pipe_to_buffer(reader, producer, inner, error_sender))
            .join()
            .is_ok()
    );
}

#[test]
fn dropping_writer_cancels_a_full_native_pipe() {
    let (reader, pipe) = anonymous(64).unwrap();
    let mut writer = EventedAnonWrite::new(pipe);

    assert_eq!(writer.write(&[0; 65536]).unwrap(), 65536);
    assert!(wait_until(|| !writer.producer.is_full()));

    let (closed_tx, closed_rx) = channel();

    let closer = spawn(move || {
        drop(writer);
        closed_tx.send(()).unwrap();
    });

    let closed = closed_rx.recv_timeout(Duration::from_secs(1)).is_ok();

    // Release the native write even on failure so the regression cannot leave
    // its helper thread blocked after the assertion.
    drop(reader);
    closer.join().unwrap();

    assert!(closed, "writer teardown waited for the native pipe reader");
}

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
