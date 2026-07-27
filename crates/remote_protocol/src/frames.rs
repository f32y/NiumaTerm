use serde::Serialize;
use serde::de::DeserializeOwned;

/// Upper bound for Control/Output/Input payloads. A Noise transport message
/// caps at 65535 bytes including the 16-byte AEAD tag, so frames must stay
/// comfortably below that; producers (the host output pump, file-sized
/// pastes) chunk larger data across multiple frames.
pub const MAX_DATA_LEN: usize = 32 * 1024;

const TYPE_CONTROL: u8 = 0x00;
const TYPE_OUTPUT: u8 = 0x01;
const TYPE_INPUT: u8 = 0x02;
const TYPE_RESIZE: u8 = 0x03;
const TYPE_EXITED: u8 = 0x04;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    /// Postcard-encoded `HostBound` or `ClientBound`; direction is implied by
    /// which side sent it, so the frame layer keeps the payload opaque.
    Control(Vec<u8>),
    Output {
        session_id: u64,
        seq: u64,
        data: Vec<u8>,
    },
    Input {
        session_id: u64,
        data: Vec<u8>,
    },
    Resize {
        session_id: u64,
        cols: u16,
        rows: u16,
    },
    Exited {
        session_id: u64,
        seq: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FrameError {
    #[error("unknown frame type {0:#04x}")]
    UnknownType(u8),
    #[error("frame truncated")]
    Truncated,
    #[error("frame payload exceeds {MAX_DATA_LEN} bytes")]
    TooLarge,
    #[error("malformed control payload: {0}")]
    Control(String),
}

impl Frame {
    pub fn control<T: Serialize>(msg: &T) -> Result<Frame, FrameError> {
        let payload = postcard::to_stdvec(msg).map_err(|e| FrameError::Control(e.to_string()))?;
        if payload.len() > MAX_DATA_LEN {
            return Err(FrameError::TooLarge);
        }
        Ok(Frame::Control(payload))
    }

    /// Decode a `Control` payload as `HostBound` or `ClientBound` depending on
    /// which direction the caller is reading.
    pub fn parse_control<T: DeserializeOwned>(payload: &[u8]) -> Result<T, FrameError> {
        postcard::from_bytes(payload).map_err(|e| FrameError::Control(e.to_string()))
    }

    pub fn encode(&self) -> Result<Vec<u8>, FrameError> {
        let mut out = Vec::new();
        match self {
            Frame::Control(payload) => {
                if payload.len() > MAX_DATA_LEN {
                    return Err(FrameError::TooLarge);
                }
                out.push(TYPE_CONTROL);
                out.extend_from_slice(payload);
            }
            Frame::Output {
                session_id,
                seq,
                data,
            } => {
                if data.len() > MAX_DATA_LEN {
                    return Err(FrameError::TooLarge);
                }
                out.push(TYPE_OUTPUT);
                out.extend_from_slice(&session_id.to_le_bytes());
                out.extend_from_slice(&seq.to_le_bytes());
                out.extend_from_slice(data);
            }
            Frame::Input { session_id, data } => {
                if data.len() > MAX_DATA_LEN {
                    return Err(FrameError::TooLarge);
                }
                out.push(TYPE_INPUT);
                out.extend_from_slice(&session_id.to_le_bytes());
                out.extend_from_slice(data);
            }
            Frame::Resize {
                session_id,
                cols,
                rows,
            } => {
                out.push(TYPE_RESIZE);
                out.extend_from_slice(&session_id.to_le_bytes());
                out.extend_from_slice(&cols.to_le_bytes());
                out.extend_from_slice(&rows.to_le_bytes());
            }
            Frame::Exited { session_id, seq } => {
                out.push(TYPE_EXITED);
                out.extend_from_slice(&session_id.to_le_bytes());
                out.extend_from_slice(&seq.to_le_bytes());
            }
        }
        Ok(out)
    }

    pub fn decode(buf: &[u8]) -> Result<Frame, FrameError> {
        let (&kind, rest) = buf.split_first().ok_or(FrameError::Truncated)?;
        match kind {
            TYPE_CONTROL => {
                if rest.len() > MAX_DATA_LEN {
                    return Err(FrameError::TooLarge);
                }
                Ok(Frame::Control(rest.to_vec()))
            }
            TYPE_OUTPUT => {
                let (session_id, rest) = read_u64(rest)?;
                let (seq, data) = read_u64(rest)?;
                if data.len() > MAX_DATA_LEN {
                    return Err(FrameError::TooLarge);
                }
                Ok(Frame::Output {
                    session_id,
                    seq,
                    data: data.to_vec(),
                })
            }
            TYPE_INPUT => {
                let (session_id, data) = read_u64(rest)?;
                if data.len() > MAX_DATA_LEN {
                    return Err(FrameError::TooLarge);
                }
                Ok(Frame::Input {
                    session_id,
                    data: data.to_vec(),
                })
            }
            TYPE_RESIZE => {
                let (session_id, rest) = read_u64(rest)?;
                let (cols, rest) = read_u16(rest)?;
                let (rows, rest) = read_u16(rest)?;
                if !rest.is_empty() {
                    return Err(FrameError::Truncated);
                }
                Ok(Frame::Resize {
                    session_id,
                    cols,
                    rows,
                })
            }
            TYPE_EXITED => {
                let (session_id, rest) = read_u64(rest)?;
                let (seq, rest) = read_u64(rest)?;
                if !rest.is_empty() {
                    return Err(FrameError::Truncated);
                }
                Ok(Frame::Exited { session_id, seq })
            }
            other => Err(FrameError::UnknownType(other)),
        }
    }
}

fn read_u64(buf: &[u8]) -> Result<(u64, &[u8]), FrameError> {
    if buf.len() < 8 {
        return Err(FrameError::Truncated);
    }
    let (head, rest) = buf.split_at(8);
    Ok((u64::from_le_bytes(head.try_into().unwrap()), rest))
}

fn read_u16(buf: &[u8]) -> Result<(u16, &[u8]), FrameError> {
    if buf.len() < 2 {
        return Err(FrameError::Truncated);
    }
    let (head, rest) = buf.split_at(2);
    Ok((u16::from_le_bytes(head.try_into().unwrap()), rest))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ClientBound, HostBound, WireSessionOptions, WireSessionSnapshot};

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
            HostBound::Open(WireSessionOptions {
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

        let msg = ClientBound::Attached(WireSessionSnapshot {
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
}
