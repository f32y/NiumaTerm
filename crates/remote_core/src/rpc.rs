//! Control messages on stream 0: JSON-RPC 2.0 message shapes without
//! batching. Error codes are names instead of numbers so a peer can match
//! on them without a shared numeric table.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::Error;

pub const TERMINAL_OPEN: &str = "terminal.open";
pub const TERMINAL_ATTACH: &str = "terminal.attach";
pub const TERMINAL_RESIZE: &str = "terminal.resize";
pub const TERMINAL_CLOSE: &str = "terminal.close";
pub const STREAM_CLOSE: &str = "stream.close";
pub const SESSIONS_LIST: &str = "sessions.list";

/// Notification: the host's session list changed; clients list it again.
pub const SESSIONS_CHANGED: &str = "sessions.changed";

/// Open a view of an agent session: the reply includes a snapshot, and
/// `agent.ops` notifications follow until the view detaches.
pub const AGENT_ATTACH: &str = "agent.attach";

/// Paged views require these methods; clients never retry with a full snapshot.
pub const AGENT_ATTACH_PAGED: &str = "agent.attach.paged";

pub const AGENT_PAGE: &str = "agent.page";
pub const AGENT_PAGED_OPS: &str = "agent.paged_ops";

pub const AGENT_DETACH: &str = "agent.detach";

/// Run a command against an agent session and return its outcome.
pub const AGENT_CALL: &str = "agent.call";

/// Notification: changes to an attached agent view.
pub const AGENT_OPS: &str = "agent.ops";

/// What a device may start on the host: its agent profiles and the
/// workspaces open there. Only names and paths travel; credentials, hooks,
/// and agent binaries stay on the host.
pub const HOST_INFO: &str = "host.info";

/// Start an agent tab on the host from a profile and a workspace that
/// `host.info` lists. The reply names the new session.
pub const AGENT_OPEN: &str = "agent.open";

/// Notification: the host ended this device's views of a session. Only one
/// side controls a session at a time, so the host taking it back ends the
/// device's views, as does the host closing the session.
pub const SESSION_ENDED: &str = "session.ended";

/// End any session the host lists, host tabs included: a headless terminal
/// ends at once, and the host closes a tab's pane as if its user had. Unlike
/// `terminal.close` this is a request, so the device learns whether the host
/// closed the session or refused, as it does when closing would take the
/// window's last workspace with it.
pub const SESSION_CLOSE: &str = "session.close";

/// Rename any session the host lists: the host renames a tab as if its user
/// had, and a headless terminal takes the name directly. Every device then
/// lists the session under the new name.
pub const SESSION_RENAME: &str = "session.rename";

/// Start a terminal tab on the host in a workspace that `host.info` lists,
/// on the host's default profile. Unlike `terminal.open`, whose shell runs
/// headless and belongs to no workspace, the tab sits in that workspace for
/// the person at the host and every device. The reply names the new session.
pub const TERMINAL_OPEN_TAB: &str = "terminal.open_tab";

/// Rename the host itself. The host takes the name as if its user had set
/// it in settings, and sends `host.renamed` to every connected device. Hosts
/// from before this method answer `unsupported`.
pub const HOST_RENAME: &str = "host.rename";

/// Notification: the host goes by a new name. Devices that were offline
/// read it from the next handshake's `HostHello` instead.
pub const HOST_RENAMED: &str = "host.renamed";

/// Exchange STUN candidates on a relay channel to open a direct path. The
/// client sends a `DirectOffer` and the host answers with its own; hosts
/// from before this method answer `unsupported`.
pub const DIRECT_OFFER: &str = "direct.offer";

#[derive(Clone, Debug, PartialEq)]
pub enum Control {
    Request {
        id: u64,
        method: String,
        params: Value,
    },
    Response {
        id: u64,
        outcome: Result<Value, RpcError>,
    },
    Notification {
        method: String,
        params: Value,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    NotFound,
    InvalidParams,
    Unsupported,
    Busy,
    Denied,
    Internal,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RpcError {
    pub code: ErrorCode,
    pub message: String,
}

impl RpcError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// The union of every field the three shapes use, so one decode classifies a
/// message by which fields are present.
#[derive(Default, Serialize, Deserialize)]
struct Raw {
    jsonrpc: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    method: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    params: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<RpcError>,
}

impl Control {
    pub fn encode(&self) -> Vec<u8> {
        let mut raw = Raw {
            jsonrpc: "2.0".into(),
            ..Raw::default()
        };

        match self {
            Control::Request { id, method, params } => {
                raw.id = Some(*id);
                raw.method = Some(method.clone());
                raw.params = Some(params.clone());
            }
            Control::Response { id, outcome } => {
                raw.id = Some(*id);

                match outcome {
                    Ok(result) => raw.result = Some(result.clone()),
                    Err(error) => raw.error = Some(error.clone()),
                }
            }
            Control::Notification { method, params } => {
                raw.method = Some(method.clone());
                raw.params = Some(params.clone());
            }
        }

        serde_json::to_vec(&raw).expect("JSON values always serialize")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        let raw: Raw = serde_json::from_slice(bytes)?;
        let params = raw.params.unwrap_or(Value::Null);

        match (raw.id, raw.method, raw.result, raw.error) {
            (Some(id), Some(method), None, None) => Ok(Control::Request { id, method, params }),
            (None, Some(method), None, None) => Ok(Control::Notification { method, params }),
            (Some(id), None, Some(result), None) => Ok(Control::Response {
                id,
                outcome: Ok(result),
            }),
            (Some(id), None, None, Some(error)) => Ok(Control::Response {
                id,
                outcome: Err(error),
            }),
            _ => Err(Error::Malformed("control message")),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalOpen {
    pub cols: u16,
    pub rows: u16,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionEnded {
    pub session: String,
    pub reason: EndReason,
}

/// Why the host ended a device's views of a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndReason {
    /// The person at the host took the session back; it keeps running
    /// there, and the device may take it again.
    TakenBack,
    /// The session is gone from the host.
    Closed,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostInfo {
    pub agents: Vec<AgentProfileInfo>,
    pub workspaces: Vec<WorkspaceInfo>,

    /// The names of the host's terminal profiles, the one new terminal
    /// tabs use by default first. Hosts from before `terminal.open_tab`
    /// send none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub terminals: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentProfileInfo {
    pub name: String,

    /// The agent the profile runs, as a session's `harness` names it.
    pub harness: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    /// The id a session's `SessionWorkspace` names this workspace by, so a
    /// device can group sessions under it even when two workspaces share a
    /// name. Hosts from before workspace ids send none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,

    pub name: String,

    /// The workspace's primary directory on the host.
    pub path: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentOpen {
    pub profile: String,

    /// A workspace path from `host.info`. The host refuses any other, so a
    /// device can start agents only where the host user already works.
    pub workspace: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalOpenTab {
    /// A workspace path from `host.info`. The host refuses any other, so a
    /// device can start shells only where the host user already works.
    pub workspace: String,

    /// A terminal profile name from `host.info`; none starts the host's
    /// default profile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRef {
    pub session: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRename {
    pub session: String,
    pub title: String,
}

/// The parameters of `host.rename` and `host.renamed`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostName {
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attached {
    pub stream: u32,
    pub cols: u16,
    pub rows: u16,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalResize {
    pub session: String,
    pub cols: u16,
    pub rows: u16,
}

/// Payload of an EXIT frame. The code is absent when the host could not
/// observe it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Exit {
    pub code: Option<i32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamRef {
    pub stream: u32,
}

/// Where a host session came from. Only remote-created sessions can be ended
/// remotely; a host tab belongs to the person at the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    Tab,
    Remote,
    #[serde(other)]
    Unknown,
}

/// What a host session runs. Peers from before agent sessions send no kind,
/// and every session they know is a terminal.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionKind {
    #[default]
    Terminal,
    Agent,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub session: String,
    pub title: String,
    pub origin: Origin,
    pub cols: u16,
    pub rows: u16,

    #[serde(default)]
    pub kind: SessionKind,

    /// The agent harness, for an agent session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,

    /// The host workspace whose tab shows the session. Hosts without
    /// workspaces, and sessions no tab shows, send none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<SessionWorkspace>,

    /// A host tab still asleep: listed, but its shell or agent starts only
    /// once someone opens it. Omitted while false, and hosts from before the
    /// flag send none, so their sessions all read as running.
    #[serde(default, skip_serializing_if = "is_false")]
    pub pending: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// A workspace on the host, as devices group sessions by it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionWorkspace {
    /// Stable while the host runs, so a renamed workspace stays one group.
    pub id: String,

    pub name: String,

    /// Where the host lists the workspace, first at 0.
    pub position: u32,
}

/// An agent command. Commands and outcomes are opaque here: the agent
/// sessions on both sides define them, so the transport never changes when
/// they do.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentCall {
    pub session: String,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentAttached {
    pub view: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentOps {
    pub session: String,
    pub ops: Value,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionList {
    pub sessions: Vec<SessionInfo>,
}
