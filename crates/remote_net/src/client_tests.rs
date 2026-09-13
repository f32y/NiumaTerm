use std::sync::mpsc as std_mpsc;

use tokio::sync::mpsc;

use crate::client::{RemoteSession, SessionByteEvent};
use crate::protocol::{Frame, ProtocolSessionSnapshot};

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
fn remote_pty_rejects_writes_and_resizes_after_the_pump_exits() {
    use std::io::{ErrorKind, Write};

    use nmt_platform::{ProcessReadWrite, WinsizeBuilder};

    use crate::net_pty::NetPty;

    let (_output_tx, output) = std_mpsc::channel();
    let (commands, mut command_rx) = mpsc::unbounded_channel();

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
    });

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
}
