use std::sync::{Arc, mpsc as std_mpsc};
use std::thread;
use std::time::Duration;

use parking_lot::Mutex;
use tokio::net::TcpListener;
use tokio::runtime::Builder as RuntimeBuilder;
use tokio::sync::{mpsc, watch};
use tokio::time;

use crate::client::{ClientWorker, RemoteSession, SessionByteEvent, reconnect};
use crate::protocol::{Frame, ProtocolSessionSnapshot, generate_keypair};

#[test]
fn reconnect_starts_without_an_initial_delay() {
    RuntimeBuilder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("ws://{}", listener.local_addr().unwrap());
            let device = generate_keypair().unwrap();
            let host = generate_keypair().unwrap();
            let (_commands, receiver) = mpsc::unbounded_channel();

            time::timeout(Duration::from_secs(1), async {
                tokio::select! {
                    _ = reconnect(&url, "host", &host.public, &device, 1, &receiver) => {
                        panic!("reconnect ended before connecting");
                    }

                    accepted = listener.accept() => {
                        accepted.unwrap();
                    }
                }
            })
            .await
            .expect("the first retry should connect immediately");
        });
}

#[test]
fn splitting_session_moves_snapshot_and_preserves_both_stream_directions() {
    let vt = vec![b'x'; 1024 * 1024];
    let allocation = vt.as_ptr();
    let (output_tx, output) = std_mpsc::channel();
    let (commands, mut command_rx) = mpsc::unbounded_channel();

    let session = RemoteSession {
        session_id: 42,
        snapshot: ProtocolSessionSnapshot {
            session_id: 42,
            base_seq: 7,
            vt,
            cols: 90,
            rows: 25,
        },
        output,
        commands,
        worker: Arc::new(ClientWorker {
            cancel: watch::channel(false).0,
            thread: Mutex::new(None),
        }),
    };

    let (snapshot, input, output): (_, _, _) = session.into();

    assert_eq!(
        snapshot.vt.as_ptr(),
        allocation,
        "the checkpoint allocation is transferred"
    );
    assert_eq!(
        (snapshot.base_seq, snapshot.cols, snapshot.rows),
        (7, 90, 25)
    );

    assert!(input.send_input(b"pwd\r".to_vec()));
    assert!(input.send_resize(100, 30));

    assert_eq!(
        command_rx.try_recv().unwrap(),
        Frame::Input {
            session_id: 42,
            data: b"pwd\r".to_vec(),
        }
    );
    assert_eq!(
        command_rx.try_recv().unwrap(),
        Frame::Resize {
            session_id: 42,
            cols: 100,
            rows: 30,
        }
    );

    output_tx
        .send(SessionByteEvent::Output(b"tail".to_vec()))
        .unwrap();

    output_tx.send(SessionByteEvent::Exited).unwrap();

    assert!(matches!(output.recv().unwrap(), SessionByteEvent::Output(bytes) if bytes == b"tail"));
    assert!(matches!(output.recv().unwrap(), SessionByteEvent::Exited));

    drop(command_rx);

    assert!(!input.send_input(b"pwd\r".to_vec()));
    assert!(!input.send_resize(80, 24));
}

#[cfg(windows)]
#[test]
fn remote_pty_reports_closed_input_and_joins_workers_on_drop() {
    use std::io::{ErrorKind, Write};

    use nmt_platform::{ProcessReadWrite, WinsizeBuilder};

    use crate::net_pty::NetPty;

    let (output_tx, output) = std_mpsc::channel();
    let (commands, mut command_rx) = mpsc::unbounded_channel();
    let (cancel, mut stopped) = watch::channel(false);
    let (finished, completion) = std_mpsc::channel();

    let thread = thread::spawn(move || {
        RuntimeBuilder::new_current_thread()
            .build()
            .unwrap()
            .block_on(async {
                let _ = stopped.wait_for(|cancelled| *cancelled).await;
            });

        drop(output_tx);
        finished.send(()).unwrap();
    });

    let mut pty = NetPty::new(RemoteSession {
        session_id: 42,
        snapshot: ProtocolSessionSnapshot {
            session_id: 42,
            base_seq: 0,
            vt: Vec::new(),
            cols: 80,
            rows: 24,
        },
        output,
        commands,
        worker: Arc::new(ClientWorker {
            cancel,
            thread: Mutex::new(Some(thread)),
        }),
    })
    .unwrap();

    assert_eq!(pty.writer().write(b"pwd\r").unwrap(), 4);
    assert!(matches!(command_rx.try_recv(), Ok(Frame::Input { data, .. }) if data == b"pwd\r"));

    drop(command_rx);

    assert_eq!(
        pty.writer().write(b"x").unwrap_err().kind(),
        ErrorKind::BrokenPipe
    );
    assert_eq!(
        pty.set_winsize(WinsizeBuilder {
            cols: 80,
            rows: 24,
            width: 0,
            height: 0,
        })
        .unwrap_err()
        .kind(),
        ErrorKind::BrokenPipe
    );

    drop(pty);

    assert!(
        completion.try_recv().is_ok(),
        "closing the PTY must join the idle client"
    );
}
