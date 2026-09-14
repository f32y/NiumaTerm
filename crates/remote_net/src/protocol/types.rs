use serde::{Deserialize, Serialize};

#[cfg(windows)]
use crate::hub::SessionInfo;
use crate::session::SessionSnapshot;

/// Options a remote client may request when opening a session. Deliberately a
/// strict subset of the hub's `SessionOptions`: environment overrides, args,
/// and process-tree management stay host-local so a paired device cannot
/// smuggle arbitrary spawn parameters past whatever the host UI allows.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ProtocolSessionOptions {
    /// `None` means the host's default shell.
    pub shell: Option<String>,

    pub working_directory: Option<String>,
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolSessionInfo {
    pub session_id: u64,
    pub shell: String,
    pub title: String,
    pub exited: bool,
    pub attached_clients: u32,
}

/// Control messages travelling client → host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HostBound {
    ListSessions,
    Open(ProtocolSessionOptions),
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
    SessionList(Vec<ProtocolSessionInfo>),
    Opened {
        session_id: u64,
    },
    Attached(SessionSnapshot),
    Paired,
    Error {
        session_id: Option<u64>,
        message: String,
    },
}

#[cfg(windows)]
impl From<SessionInfo> for ProtocolSessionInfo {
    fn from(info: SessionInfo) -> Self {
        ProtocolSessionInfo {
            session_id: info.id.0,
            shell: info.shell,
            title: info.title.unwrap_or_default(),
            exited: info.exited,
            attached_clients: info.attached_clients as u32,
        }
    }
}
