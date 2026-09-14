use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(pub u64);

impl fmt::Display for SessionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Reconnect checkpoint: everything the client needs to rebuild terminal
/// state. Bytes are either inside `vt` or arrive in Output frames with
/// `seq > base_seq` — never both, never neither. All chunks of one output
/// event share its sequence number and must be applied together.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub session_id: SessionId,
    pub base_seq: u64,
    pub vt: Vec<u8>,
    pub cols: u16,
    pub rows: u16,
}
