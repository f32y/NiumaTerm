use std::fs::File;
use std::io::{self, Read, Write};
use std::sync::Arc;
use std::sync::mpsc::channel;
use std::thread::spawn;
use std::time::{Duration, Instant};

use mio::{Events, Poll, Token, Waker};

use crate::windows::pipes::{ConinPipe, PIPE_BUFFER, conin_pair, conout_pair};

/// A writer whose second write cannot complete: the first one filled the pipe
/// buffer and the peer has not read anything yet.
fn writer_with_pending_write() -> (File, ConinPipe) {
    let (peer, mut writer) = conin_pair().unwrap();

    assert_eq!(writer.write(&[0x5a; PIPE_BUFFER]).unwrap(), PIPE_BUFFER);
    assert_eq!(writer.write(&[0xa5; PIPE_BUFFER]).unwrap(), PIPE_BUFFER);

    (File::from(peer), writer)
}

#[test]
fn flush_waits_for_native_writes_and_completion_wakes_the_loop() {
    let (mut peer, mut writer) = writer_with_pending_write();

    let mut poll = Poll::new().unwrap();

    writer
        .soft()
        .set_waker(Arc::new(Waker::new(poll.registry(), Token(0)).unwrap()));

    let mut events = Events::with_capacity(4);

    assert_eq!(
        writer.flush().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    assert_eq!(
        writer.write(b"later").unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );

    let drain = spawn(move || {
        let mut received = vec![0; 2 * PIPE_BUFFER];

        peer.read_exact(&mut received).unwrap();

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

    let received = drain.join().unwrap();

    assert!(received[..PIPE_BUFFER].iter().all(|byte| *byte == 0x5a));
    assert!(received[PIPE_BUFFER..].iter().all(|byte| *byte == 0xa5));
}

#[test]
fn native_write_failure_wakes_a_pending_flush() {
    let (peer, mut writer) = writer_with_pending_write();

    let mut poll = Poll::new().unwrap();

    writer
        .soft()
        .set_waker(Arc::new(Waker::new(poll.registry(), Token(0)).unwrap()));

    let mut events = Events::with_capacity(4);

    assert_eq!(
        writer.flush().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );

    drop(peer);

    loop {
        events.clear();

        poll.poll(&mut events, Some(Duration::from_secs(2)))
            .unwrap();

        assert!(
            !events.is_empty(),
            "native failure did not wake the parked loop"
        );

        // The kernel may report the abandoned write either as failed or as
        // finished; what the loop needs is that the wait ends either way.
        match writer.flush() {
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => break,
            Err(error) => panic!("unexpected native write error: {error}"),
            Ok(()) => break,
        }
    }

    assert_eq!(
        writer.write(b"after").unwrap_err().kind(),
        io::ErrorKind::BrokenPipe
    );
}

#[test]
fn dropping_writer_cancels_a_pending_native_write() {
    let (peer, writer) = writer_with_pending_write();

    let (closed_tx, closed_rx) = channel();

    let closer = spawn(move || {
        drop(writer);

        closed_tx.send(()).unwrap();
    });

    let closed = closed_rx.recv_timeout(Duration::from_secs(1)).is_ok();

    // Release the native write even on failure so the regression cannot leave
    // its helper thread blocked after the assertion.
    drop(peer);

    closer.join().unwrap();

    assert!(closed, "writer teardown waited for the native pipe reader");
}

#[test]
fn an_empty_write_never_reaches_the_pipe() {
    let (peer, mut writer) = conin_pair().unwrap();
    let mut peer = File::from(peer);

    assert_eq!(writer.write(&[]).unwrap(), 0);

    writer.write_all(b"x").unwrap();

    // A zero-length native write would complete this read with no bytes.
    let mut received = [0; 1];

    assert_eq!(peer.read(&mut received).unwrap(), 1);
    assert_eq!(&received, b"x");
}

fn poll_readable(poll: &mut Poll, token: Token) {
    let deadline = Instant::now() + Duration::from_secs(2);

    let mut events = Events::with_capacity(8);

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());

        assert!(!remaining.is_zero(), "pipe did not become readable");

        poll.poll(&mut events, Some(remaining)).unwrap();

        if events
            .iter()
            .any(|event| event.token() == token && event.is_readable())
        {
            return;
        }
    }
}

/// Partial reads must retain local readiness because buffered bytes do not
/// produce another IOCP completion until the next native read starts.
#[test]
fn readiness_stays_set_until_output_is_fully_drained() {
    let (mut reader, pty_side) = conout_pair().unwrap();
    let mut pty_side = File::from(pty_side);
    let mut poll = Poll::new().unwrap();

    reader.register(&poll, Token(1)).unwrap();

    // Push more than a single small read will drain.
    pty_side.write_all(&[0xABu8; 4096]).expect("write");

    poll_readable(&mut poll, Token(1));

    // Read only a slice — data remains buffered.
    let mut small = [0u8; 16];

    let got = reader.read(&mut small).expect("partial read");

    assert!(got > 0 && got <= 16);
    assert!(
        reader.is_ready(),
        "flag must stay set while output is still buffered"
    );

    let mut drained = got;
    let mut sink = [0u8; 4096];

    while drained < 4096 {
        match reader.read(&mut sink) {
            Ok(0) => poll_readable(&mut poll, Token(1)),
            Ok(n) => drained += n,
            Err(e) => panic!("drain read failed: {e}"),
        }
    }

    assert_eq!(drained, 4096, "should read back every byte written");

    assert_eq!(reader.read(&mut sink).unwrap(), 0);
    assert!(!reader.is_ready(), "an empty pipe must let the loop park");

    let mut events = Events::with_capacity(8);

    poll.poll(&mut events, Some(Duration::from_millis(20)))
        .unwrap();

    assert!(events.is_empty(), "drained output kept the loop awake");
}

#[test]
fn output_written_before_the_first_read_is_not_lost() {
    let (mut reader, pty_side) = conout_pair().unwrap();
    let mut pty_side = File::from(pty_side);

    pty_side.write_all(b"early").unwrap();

    let mut poll = Poll::new().unwrap();

    reader.register(&poll, Token(1)).unwrap();

    poll_readable(&mut poll, Token(1));

    let mut sink = [0u8; 64];

    let got = reader.read(&mut sink).unwrap();

    assert_eq!(&sink[..got], b"early");
}

#[test]
fn a_closed_peer_is_reported_as_a_broken_pipe() {
    let (mut reader, pty_side) = conout_pair().unwrap();
    let mut poll = Poll::new().unwrap();

    reader.register(&poll, Token(1)).unwrap();

    drop(pty_side);
    poll_readable(&mut poll, Token(1));

    let mut sink = [0u8; 64];

    assert_eq!(
        reader.read(&mut sink).unwrap_err().kind(),
        io::ErrorKind::BrokenPipe
    );
}

#[test]
fn read_completions_use_the_pipe_token_without_a_waker() {
    let (mut reader, peer) = conout_pair().unwrap();
    let mut peer = File::from(peer);
    let mut poll = Poll::new().unwrap();

    reader.register(&poll, Token(7)).unwrap();

    let mut sink = [0; 64];

    for payload in [b"first".as_slice(), b"second".as_slice()] {
        assert_eq!(reader.read(&mut sink).unwrap(), 0);
        assert!(!reader.is_ready());

        peer.write_all(payload).unwrap();

        poll_readable(&mut poll, Token(7));

        let got = reader.read(&mut sink).unwrap();

        assert_eq!(&sink[..got], payload);
    }
}

#[test]
fn an_empty_read_keeps_buffered_output_ready() {
    let (mut reader, peer) = conout_pair().unwrap();
    let mut peer = File::from(peer);
    let mut poll = Poll::new().unwrap();

    reader.register(&poll, Token(1)).unwrap();

    peer.write_all(b"xy").unwrap();

    poll_readable(&mut poll, Token(1));

    let mut byte = [0; 1];

    assert_eq!(reader.read(&mut byte).unwrap(), 1);
    assert_eq!(&byte, b"x");
    assert_eq!(reader.read(&mut []).unwrap(), 0);
    assert!(reader.is_ready());
    assert_eq!(reader.read(&mut byte).unwrap(), 1);
    assert_eq!(&byte, b"y");
}

#[test]
fn a_zero_length_peer_write_does_not_close_output() {
    use std::os::windows::io::AsRawHandle;
    use std::ptr;

    use windows_sys::Win32::Storage::FileSystem::WriteFile;

    let (mut reader, peer) = conout_pair().unwrap();
    let mut peer = File::from(peer);
    let mut poll = Poll::new().unwrap();

    reader.register(&poll, Token(1)).unwrap();

    let mut written = 0;

    // SAFETY: The peer owns a synchronous pipe handle, the empty buffer is
    // valid for zero bytes, and the byte-count output lives through the call.
    assert_ne!(
        unsafe {
            WriteFile(
                peer.as_raw_handle(),
                b"".as_ptr(),
                0,
                &mut written,
                ptr::null_mut(),
            )
        },
        0
    );

    let mut events = Events::with_capacity(8);

    poll.poll(&mut events, Some(Duration::from_millis(20)))
        .unwrap();

    let mut sink = [0; 64];

    assert_eq!(reader.read(&mut sink).unwrap(), 0);
    assert!(!reader.is_ready());

    peer.write_all(b"still open").unwrap();

    poll_readable(&mut poll, Token(1));

    let got = reader.read(&mut sink).unwrap();

    assert_eq!(&sink[..got], b"still open");
}

#[test]
fn deregistration_retains_completed_output_for_the_next_registration() {
    let (mut reader, peer) = conout_pair().unwrap();
    let mut peer = File::from(peer);
    let mut poll = Poll::new().unwrap();

    reader.register(&poll, Token(1)).unwrap();
    reader.deregister(&poll).unwrap();

    assert!(!reader.is_ready());

    peer.write_all(b"retained").unwrap();

    let mut events = Events::with_capacity(8);

    poll.poll(&mut events, Some(Duration::from_millis(20)))
        .unwrap();

    assert!(
        events.is_empty(),
        "deregistered output still emitted readiness"
    );

    reader.register(&poll, Token(2)).unwrap();

    assert!(reader.is_ready());

    let mut sink = [0; 64];

    let got = reader.read(&mut sink).unwrap();

    assert_eq!(&sink[..got], b"retained");

    assert_eq!(reader.read(&mut sink).unwrap(), 0);

    peer.write_all(b"next").unwrap();

    poll_readable(&mut poll, Token(2));

    let got = reader.read(&mut sink).unwrap();

    assert_eq!(&sink[..got], b"next");
}

#[test]
fn dropping_reader_cancels_a_pending_native_read() {
    let (mut reader, peer) = conout_pair().unwrap();
    let mut poll = Poll::new().unwrap();

    reader.register(&poll, Token(1)).unwrap();

    let (closed_tx, closed_rx) = channel();

    let closer = spawn(move || {
        drop(reader);

        closed_tx.send(()).unwrap();
    });

    let closed = closed_rx.recv_timeout(Duration::from_secs(1)).is_ok();

    // Closing the peer releases a stuck read even if cancellation regresses.
    drop(peer);

    closer.join().unwrap();

    assert!(closed, "reader teardown waited for native output");

    // Process the cancellation while its overlapped storage is still owned by mio.
    let mut events = Events::with_capacity(8);

    poll.poll(&mut events, Some(Duration::from_millis(20)))
        .unwrap();
}

#[test]
#[ignore = "temporary latency probe"]
fn zz_probe_wake_latency() {
    use std::os::windows::io::{FromRawHandle as _, IntoRawHandle as _};
    use std::time::Instant;

    use mio::Interest;
    use mio::windows::NamedPipe;
    use windows_sys::Win32::Foundation::GENERIC_WRITE;
    use windows_sys::Win32::Storage::FileSystem::PIPE_ACCESS_INBOUND;

    use crate::windows::pipes::pipe_pair;

    const ROUNDS: usize = 2000;

    fn report(name: &str, mut samples: Vec<u128>) {
        samples.sort_unstable();

        eprintln!(
            "{name}: p50={}us p90={}us p99={}us",
            samples[samples.len() / 2] / 1000,
            samples[samples.len() * 9 / 10] / 1000,
            samples[samples.len() * 99 / 100] / 1000,
        );
    }

    // ConPTY wrapper: completion packet directly to the polled port.
    {
        let (mut reader, peer) = conout_pair().unwrap();
        let mut peer = File::from(peer);
        let mut poll = Poll::new().unwrap();

        reader.register(&poll, Token(1)).unwrap();

        let mut events = Events::with_capacity(8);
        let mut sink = [0u8; 4096];
        let mut samples = Vec::new();

        while reader.read(&mut sink).unwrap() > 0 {}

        for _ in 0..ROUNDS {
            let started = Instant::now();

            peer.write_all(b"x").unwrap();

            loop {
                if reader.read(&mut sink).unwrap() > 0 {
                    break;
                }

                events.clear();

                poll.poll(&mut events, Some(Duration::from_secs(1)))
                    .unwrap();
            }

            samples.push(started.elapsed().as_nanos());

            while reader.read(&mut sink).unwrap() > 0 {}

            events.clear();

            poll.poll(&mut events, Some(Duration::ZERO)).unwrap();
        }

        report("ConoutPipe IOCP", samples);
    }

    // mio NamedPipe: completion packet straight to the polled port.
    {
        let (ours, peer) = pipe_pair(PIPE_ACCESS_INBOUND, GENERIC_WRITE).unwrap();

        let mut peer = File::from(peer);

        let mut pipe = unsafe { NamedPipe::from_raw_handle(ours.into_raw_handle()) };

        let mut poll = Poll::new().unwrap();

        poll.registry()
            .register(&mut pipe, Token(1), Interest::READABLE)
            .unwrap();

        let mut events = Events::with_capacity(8);
        let mut sink = [0u8; 4096];
        let mut samples = Vec::new();

        for _ in 0..ROUNDS {
            let started = Instant::now();

            peer.write_all(b"x").unwrap();

            loop {
                match pipe.read(&mut sink) {
                    Ok(n) if n > 0 => break,
                    _ => {}
                }

                events.clear();

                poll.poll(&mut events, Some(Duration::from_secs(1)))
                    .unwrap();
            }

            samples.push(started.elapsed().as_nanos());
        }

        report("mio NamedPipe", samples);
    }

    // Flood: 64 MiB written by a thread, time to drain.
    for mode in ["ours", "mio"] {
        const TOTAL: usize = 64 * 1024 * 1024;

        let started;

        let mut polls = 0u64;
        let mut got = 0usize;
        let mut sink = vec![0u8; 1 << 20];
        let mut events = Events::with_capacity(8);
        let mut poll = Poll::new().unwrap();

        let writer = |mut peer: File| {
            spawn(move || {
                let chunk = [0u8; 4096];

                for _ in 0..TOTAL / 4096 {
                    peer.write_all(&chunk).unwrap();
                }
            })
        };

        if mode == "ours" {
            let (mut reader, peer) = conout_pair().unwrap();

            reader.register(&poll, Token(1)).unwrap();

            started = Instant::now();

            let thread = writer(File::from(peer));

            while got < TOTAL {
                let n = reader.read(&mut sink).unwrap();

                if n == 0 {
                    events.clear();

                    polls += 1;

                    poll.poll(&mut events, Some(Duration::from_secs(1)))
                        .unwrap();
                }

                got += n;
            }

            thread.join().unwrap();
        } else {
            let (ours, peer) = pipe_pair(PIPE_ACCESS_INBOUND, GENERIC_WRITE).unwrap();

            let mut pipe = unsafe { NamedPipe::from_raw_handle(ours.into_raw_handle()) };

            poll.registry()
                .register(&mut pipe, Token(1), Interest::READABLE)
                .unwrap();

            started = Instant::now();

            let thread = writer(File::from(peer));

            while got < TOTAL {
                match pipe.read(&mut sink) {
                    Ok(n) => got += n,
                    Err(_) => {
                        events.clear();

                        polls += 1;

                        poll.poll(&mut events, Some(Duration::from_secs(1)))
                            .unwrap();
                    }
                }
            }

            thread.join().unwrap();
        }

        eprintln!(
            "flood {mode}: {:?} for 64 MiB, {polls} polls",
            started.elapsed()
        );
    }
}
