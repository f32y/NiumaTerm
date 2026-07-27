use serde::{Deserialize, Serialize};

/// Options a remote client may request when opening a session. Deliberately a
/// strict subset of the hub's `SessionOptions`: environment overrides, args,
/// and process-tree management stay host-local so a paired device cannot
/// smuggle arbitrary spawn parameters past whatever the host UI allows.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WireSessionOptions {
    /// `None` means the host's default shell.
    pub shell: Option<String>,
    pub working_directory: Option<String>,
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireSessionInfo {
    pub session_id: u64,
    pub shell: String,
    pub title: String,
    pub exited: bool,
    pub attached_clients: u32,
}

/// Reconnect checkpoint: everything the client needs to rebuild terminal
/// state. Bytes are either inside `vt` or arrive in Output frames with
/// `seq >= base_seq` — never both, never neither.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireSessionSnapshot {
    pub session_id: u64,
    pub base_seq: u64,
    pub vt: Vec<u8>,
    pub cols: u16,
    pub rows: u16,
}

/// Control messages travelling client → host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HostBound {
    ListSessions,
    Open(WireSessionOptions),
    Attach {
        session_id: u64,
    },
    Detach {
        session_id: u64,
    },
    Kill {
        session_id: u64,
    },
    /// Sent inside an XX-handshake channel to redeem a one-time pairing token.
    Pair {
        token: [u8; 16],
        device_name: String,
    },
}

/// Control messages travelling host → client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientBound {
    SessionList(Vec<WireSessionInfo>),
    Opened {
        session_id: u64,
    },
    Attached(WireSessionSnapshot),
    Paired,
    Error {
        session_id: Option<u64>,
        message: String,
    },
}
