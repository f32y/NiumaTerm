use crate::protocol::frames::*;
use crate::{ClientBound, HostBound, ProtocolSessionOptions, ProtocolSessionSnapshot};

#[test]
fn data_frames_roundtrip() {
    let frames = [
        Frame::Output {
            session_id: 7,
            seq: 42,
            data: b"hello\x1b[0m".to_vec(),
        },
        Frame::Output {
            session_id: 7,
            seq: 43,
            data: Vec::new(),
        },
        Frame::Input {
            session_id: 1,
            data: b"dir\r".to_vec(),
        },
        Frame::Resize {
            session_id: u64::MAX,
            cols: 120,
            rows: 30,
        },
        Frame::Exited {
            session_id: 3,
            seq: 9000,
        },
        Frame::Control(vec![1, 2, 3]),
    ];
    for frame in frames {
        let encoded = frame.encode().unwrap();
        assert_eq!(Frame::decode(&encoded).unwrap(), frame);
    }
}

#[test]
fn control_messages_roundtrip() {
    let host_bound = [
        HostBound::ListSessions,
        HostBound::Open(ProtocolSessionOptions {
            shell: Some("pwsh.exe".into()),
            working_directory: Some(r"C:\Workspace".into()),
            cols: 120,
            rows: 30,
        }),
        HostBound::Attach { session_id: 5 },
        HostBound::Pair {
            token: [7; 16],
            device_name: "laptop".into(),
        },
    ];
    for msg in host_bound {
        let frame = Frame::control(&msg).unwrap();
        let bytes = frame.encode().unwrap();
        let Frame::Control(payload) = Frame::decode(&bytes).unwrap() else {
            panic!("expected control frame");
        };
        assert_eq!(Frame::parse_control::<HostBound>(&payload).unwrap(), msg);
    }

    let msg = ClientBound::Attached(ProtocolSessionSnapshot {
        session_id: 5,
        base_seq: 100,
        vt: b"\x1b[2J\x1b[Hprompt>".to_vec(),
        cols: 120,
        rows: 30,
    });
    let frame = Frame::control(&msg).unwrap();
    let Frame::Control(payload) = Frame::decode(&frame.encode().unwrap()).unwrap() else {
        panic!("expected control frame");
    };
    assert_eq!(Frame::parse_control::<ClientBound>(&payload).unwrap(), msg);
}

#[test]
fn unknown_type_and_truncation_are_errors() {
    assert_eq!(
        Frame::decode(&[0xff, 1, 2]),
        Err(FrameError::UnknownType(0xff))
    );
    assert_eq!(Frame::decode(&[]), Err(FrameError::Truncated));
    assert_eq!(
        Frame::decode(&[TYPE_OUTPUT, 1, 2, 3]),
        Err(FrameError::Truncated)
    );
    assert_eq!(
        Frame::decode(&[TYPE_RESIZE, 0, 0, 0, 0, 0, 0, 0, 0, 120, 0, 30, 0, 99]),
        Err(FrameError::Truncated)
    );
}

#[test]
fn oversized_payload_rejected() {
    let frame = Frame::Input {
        session_id: 1,
        data: vec![0; MAX_DATA_LEN + 1],
    };
    assert_eq!(frame.encode(), Err(FrameError::TooLarge));
}
