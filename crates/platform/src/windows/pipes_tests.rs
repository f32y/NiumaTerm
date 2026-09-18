use std::fs::File;
use std::future::poll_fn;
use std::io::{self, Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::channel;
use std::task::{Context, Poll as TaskPoll, Waker};
use std::thread::spawn;
use std::time::Duration;

use tokio::runtime::{Builder, Runtime};
use tokio::task::yield_now;
use tokio::time::{sleep, timeout};

use crate::windows::pipes::{ConinPipe, ConoutPipe, PIPE_BUFFER, conin_pair, conout_pair};

#[test]
fn async_output_parks_and_native_completion_wakes_the_reader() {
    let (mut reader, peer) = conout_pair().unwrap();
    let mut peer = File::from(peer);

    let runtime = runtime();
    let polls = Arc::new(AtomicUsize::new(0));
    let counter = polls.clone();

    runtime.block_on(async {
        let task = tokio::spawn(async move {
            let mut buf = [0; 3];

            let count = poll_fn(|cx| {
                counter.fetch_add(1, Ordering::Relaxed);

                reader.poll_read(cx, &mut buf)
            })
            .await
            .unwrap();

            (reader, buf, count)
        });

        // On a current-thread runtime, yielding runs the spawned task up to
        // its first pending poll before the peer writes anything.
        yield_now().await;

        assert_eq!(polls.load(Ordering::Relaxed), 1);

        peer.write_all(b"abc").unwrap();

        let (mut reader, buf, count) = timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();

        // One poll parked and one completed it: an idle IOCP read never
        // wakes itself.
        assert_eq!(polls.load(Ordering::Relaxed), 2);
        assert_eq!(count, 3);
        assert_eq!(&buf, b"abc");

        drop(peer);

        let error = timeout(
            Duration::from_secs(2),
            poll_fn(|cx| reader.poll_read(cx, &mut [0; 1])),
        )
        .await
        .unwrap()
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
    });
}

#[test]
fn async_flush_waits_for_native_completion_under_backpressure() {
    let (mut peer, mut writer) = writer_with_pending_write();

    runtime().block_on(async {
        poll_fn(|cx| {
            assert!(writer.poll_flush(cx).is_pending());
            assert!(writer.poll_write(cx, b"later").is_pending());

            TaskPoll::Ready(())
        })
        .await;

        let drain = spawn(move || {
            let mut bytes = vec![0; 2 * PIPE_BUFFER];

            peer.read_exact(&mut bytes).unwrap();

            (peer, bytes)
        });

        timeout(Duration::from_secs(2), poll_fn(|cx| writer.poll_flush(cx)))
            .await
            .unwrap()
            .unwrap();

        let (mut peer, received) = drain.join().unwrap();

        assert!(received[..PIPE_BUFFER].iter().all(|byte| *byte == 0x5a));
        assert!(received[PIPE_BUFFER..].iter().all(|byte| *byte == 0xa5));
        assert_eq!(
            poll_fn(|cx| writer.poll_write(cx, b"later")).await.unwrap(),
            5
        );

        let mut tail = [0; 5];

        peer.read_exact(&mut tail).unwrap();

        assert_eq!(&tail, b"later");
    });
}

/// Submit without waiting; a writer still sending reports `WouldBlock`.
fn write_now(writer: &mut ConinPipe, buf: &[u8]) -> io::Result<usize> {
    match writer.poll_write(&mut Context::from_waker(Waker::noop()), buf) {
        TaskPoll::Ready(result) => result,
        TaskPoll::Pending => Err(io::ErrorKind::WouldBlock.into()),
    }
}

/// A writer whose second write cannot complete: the first one filled the pipe
/// buffer and the peer has not read anything yet.
fn writer_with_pending_write() -> (File, ConinPipe) {
    let (peer, mut writer) = conin_pair().unwrap();

    assert_eq!(
        write_now(&mut writer, &[0x5a; PIPE_BUFFER]).unwrap(),
        PIPE_BUFFER
    );
    assert_eq!(
        write_now(&mut writer, &[0xa5; PIPE_BUFFER]).unwrap(),
        PIPE_BUFFER
    );

    (File::from(peer), writer)
}

#[test]
fn native_write_failure_wakes_a_pending_flush() {
    let (peer, mut writer) = writer_with_pending_write();

    runtime().block_on(async {
        poll_fn(|cx| {
            assert!(writer.poll_flush(cx).is_pending());

            TaskPoll::Ready(())
        })
        .await;

        drop(peer);

        // The kernel may report the abandoned write either as failed or as
        // finished; what the task needs is that the wait ends either way.
        match timeout(Duration::from_secs(2), poll_fn(|cx| writer.poll_flush(cx)))
            .await
            .expect("native failure did not wake the waiting task")
        {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {}
            Err(error) => panic!("unexpected native write error: {error}"),
        }
    });

    assert_eq!(
        write_now(&mut writer, b"after").unwrap_err().kind(),
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

    assert_eq!(write_now(&mut writer, &[]).unwrap(), 0);
    assert_eq!(write_now(&mut writer, b"x").unwrap(), 1);

    // A zero-length native write would complete this read with no bytes.
    let mut received = [0; 1];

    assert_eq!(peer.read(&mut received).unwrap(), 1);
    assert_eq!(&received, b"x");
}

fn runtime() -> Runtime {
    Builder::new_current_thread().enable_all().build().unwrap()
}

fn started_reader() -> (ConoutPipe, File) {
    let (reader, peer) = conout_pair().unwrap();

    (reader, File::from(peer))
}

async fn read(reader: &mut ConoutPipe, buf: &mut [u8]) -> io::Result<usize> {
    timeout(
        Duration::from_secs(2),
        poll_fn(|cx| reader.poll_read(cx, buf)),
    )
    .await
    .expect("pipe did not become readable")
}

/// Whether a read would park the task now, without waiting for readiness.
fn would_park(reader: &mut ConoutPipe) -> bool {
    reader
        .poll_read(&mut Context::from_waker(Waker::noop()), &mut [0; 64])
        .is_pending()
}

/// Partial reads must keep readiness because buffered bytes do not produce
/// another IOCP completion until the next native read starts.
#[test]
fn partial_reads_drain_buffered_output_before_parking() {
    let runtime = runtime();

    let (mut reader, mut peer) = started_reader();

    runtime.block_on(async {
        // Push more than a single small read will drain.
        peer.write_all(&[0xAB; 4096]).unwrap();

        let mut small = [0; 16];

        let mut drained = read(&mut reader, &mut small).await.unwrap();

        assert!(drained > 0 && drained <= 16);

        let mut sink = [0; 4096];

        while drained < 4096 {
            drained += read(&mut reader, &mut sink).await.unwrap();
        }

        assert_eq!(drained, 4096, "should read back every byte written");
        assert!(
            would_park(&mut reader),
            "an empty pipe must let the task park"
        );
    });
}

#[test]
fn output_written_before_the_first_read_is_not_lost() {
    let (mut reader, peer) = conout_pair().unwrap();
    let mut peer = File::from(peer);

    peer.write_all(b"early").unwrap();

    runtime().block_on(async {
        let mut sink = [0; 64];

        let got = read(&mut reader, &mut sink).await.unwrap();

        assert_eq!(&sink[..got], b"early");
    });
}

#[test]
fn a_closed_peer_is_reported_as_a_broken_pipe() {
    let runtime = runtime();

    let (mut reader, peer) = started_reader();

    drop(peer);

    runtime.block_on(async {
        assert_eq!(
            read(&mut reader, &mut [0; 64]).await.unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
    });
}

#[test]
fn each_payload_wakes_a_parked_read() {
    let runtime = runtime();

    let (mut reader, mut peer) = started_reader();

    runtime.block_on(async {
        let mut sink = [0; 64];

        for payload in [b"first".as_slice(), b"second".as_slice()] {
            assert!(would_park(&mut reader));

            peer.write_all(payload).unwrap();

            let got = read(&mut reader, &mut sink).await.unwrap();

            assert_eq!(&sink[..got], payload);
        }
    });
}

#[test]
fn an_empty_read_keeps_buffered_output_ready() {
    let runtime = runtime();

    let (mut reader, mut peer) = started_reader();

    runtime.block_on(async {
        peer.write_all(b"xy").unwrap();

        let mut byte = [0; 1];

        assert_eq!(read(&mut reader, &mut byte).await.unwrap(), 1);
        assert_eq!(&byte, b"x");
        assert_eq!(read(&mut reader, &mut []).await.unwrap(), 0);
        assert_eq!(read(&mut reader, &mut byte).await.unwrap(), 1);
        assert_eq!(&byte, b"y");
    });
}

#[test]
fn a_zero_length_peer_write_does_not_close_output() {
    use std::os::windows::io::AsRawHandle;
    use std::ptr;

    use windows_sys::Win32::Storage::FileSystem::WriteFile;

    let runtime = runtime();

    let (mut reader, mut peer) = started_reader();

    runtime.block_on(async {
        assert!(would_park(&mut reader));

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

        // Let the zero-byte completion reach the runtime before sampling.
        sleep(Duration::from_millis(20)).await;

        assert!(would_park(&mut reader));

        peer.write_all(b"still open").unwrap();

        let mut sink = [0; 64];

        let got = read(&mut reader, &mut sink).await.unwrap();

        assert_eq!(&sink[..got], b"still open");
    });
}

#[test]
fn dropping_reader_cancels_a_pending_native_read() {
    let runtime = runtime();

    let (mut reader, peer) = started_reader();

    // Start the native read that dropping must cancel.
    runtime.block_on(async { assert!(would_park(&mut reader)) });

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
}
