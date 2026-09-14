use std::io::{Read, Write};
use std::thread::sleep;
use std::time::Duration;

use mio::{Events, Poll, Token, Waker};
use miow::pipe::anonymous;

use crate::windows::pipes::*;

#[test]
fn flush_waits_for_native_writes_after_the_ring_has_space() {
    let (mut reader, pipe) = anonymous(64).unwrap();
    let mut writer = EventedAnonWrite::new(pipe);
    let bytes = vec![0x5a; 65536];
    let mut poll = Poll::new().unwrap();

    writer
        .soft()
        .set_waker(Arc::new(Waker::new(poll.registry(), Token(0)).unwrap()));

    let mut events = Events::with_capacity(4);

    assert_eq!(writer.write(&bytes).unwrap(), bytes.len());
    assert!(wait_until(|| !writer.producer.is_full()));

    poll.poll(&mut events, Some(Duration::ZERO)).unwrap();

    assert_eq!(
        writer.flush().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );

    let drain = spawn(move || {
        let mut received = vec![0; 65536];

        reader.read_exact(&mut received).unwrap();

        received
    });

    loop {
        events.clear();

        poll.poll(&mut events, Some(Duration::from_secs(2)))
            .unwrap();

        assert!(
            !events.is_empty(),
            "native completion did not wake the parked loop"
        );

        match writer.flush() {
            Ok(()) => break,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => panic!("native write failed: {error}"),
        }
    }

    assert_eq!(drain.join().unwrap(), bytes);
}

#[test]
fn native_write_failure_wakes_a_pending_flush() {
    let (reader, pipe) = anonymous(64).unwrap();
    let mut writer = EventedAnonWrite::new(pipe);
    let mut poll = Poll::new().unwrap();

    writer
        .soft()
        .set_waker(Arc::new(Waker::new(poll.registry(), Token(0)).unwrap()));

    let mut events = Events::with_capacity(4);

    writer.write_all(&[0; 65536]).unwrap();

    assert!(wait_until(|| !writer.producer.is_full()));

    poll.poll(&mut events, Some(Duration::ZERO)).unwrap();

    assert_eq!(
        writer.flush().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );

    drop(reader);

    loop {
        events.clear();

        poll.poll(&mut events, Some(Duration::from_secs(2)))
            .unwrap();

        assert!(
            !events.is_empty(),
            "native failure did not wake the parked loop"
        );

        match writer.flush().unwrap_err().kind() {
            io::ErrorKind::WouldBlock => {}
            io::ErrorKind::BrokenPipe => break,
            error => panic!("unexpected native write error: {error}"),
        }
    }
}

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
