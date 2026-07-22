use std::collections::HashMap;
use std::mem::{self, align_of, size_of};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;
use std::{error, fmt, ptr, str, sync, thread};

use raw_sync::Timeout;
use raw_sync::events::{Event, EventImpl, EventInit, EventState};
use serde_json::{from_slice, to_vec};
use shared_memory::{Shmem, ShmemConf, ShmemError};

use crate::{RemoteSessionHub, SessionEvent, SessionId, SessionOptions, SessionSubscription};

const MAGIC: [u8; 8] = *b"NMTSHM02";
const VERSION: u32 = 2;
const EMPTY: u32 = 0;
const READY: u32 = 1;
const HOST_SEND_TIMEOUT: Duration = Duration::from_secs(5);
const EVENT_COUNT: usize = 4;

/// Each direction gets one mailbox of this size. A mailbox keeps the protocol
/// bounded and provides backpressure instead of overwriting unread terminal data.
pub const DEFAULT_MAILBOX_CAPACITY: usize = 16 * 1024 * 1024;

#[repr(C, align(64))]
struct MailboxControl {
    state: AtomicU32,
    len: AtomicU32,
    reserved: [u8; 56],
}

impl MailboxControl {
    fn new() -> Self {
        Self {
            state: AtomicU32::new(EMPTY),
            len: AtomicU32::new(0),
            reserved: [0; 56],
        }
    }
}

#[repr(C, align(64))]
struct MappingHeader {
    magic: [u8; 8],
    version: u32,
    capacity: u32,
    event_size: u32,
    parent_to_child: MailboxControl,
    child_to_parent: MailboxControl,
}

#[derive(Clone, Copy)]
enum Role {
    Parent,
    Child,
}

struct Mailbox {
    control: *mut MailboxControl,
    data: *mut u8,
    capacity: usize,
    ready_event: Box<dyn EventImpl>,
    empty_event: Box<dyn EventImpl>,
}

/// Keep an endpoint on one IPC pump thread. The shared-memory mapping crate
/// intentionally exposes raw memory, so this type does not claim `Send` or `Sync`.
pub struct SharedMemoryEndpoint {
    mapping: Shmem,
    outbound: Mailbox,
    inbound: Mailbox,
}

impl SharedMemoryEndpoint {
    pub fn create_parent(capacity: usize) -> Result<Self, IpcError> {
        if capacity == 0 || capacity > u32::MAX as usize {
            return Err(IpcError::InvalidCapacity(capacity));
        }

        let layout = mapping_layout(capacity)?;
        let total = layout.total;
        let mapping = ShmemConf::new().size(total).create()?;
        let base = mapping.as_ptr();

        if !(base as usize).is_multiple_of(align_of::<MappingHeader>()) {
            return Err(IpcError::InvalidLayout);
        }

        unsafe {
            // The mapping is newly created and inaccessible to the child until its
            // identifier is handed over, so initializing the header has no reader.
            ptr::write(
                base.cast::<MappingHeader>(),
                MappingHeader {
                    magic: MAGIC,
                    version: VERSION,
                    capacity: capacity as u32,
                    event_size: layout.event_size as u32,
                    parent_to_child: MailboxControl::new(),
                    child_to_parent: MailboxControl::new(),
                },
            );
        }
        Self::from_mapping(mapping, Role::Parent, true)
    }

    pub fn open_child(os_id: &str) -> Result<Self, IpcError> {
        let mapping = ShmemConf::new().os_id(os_id).open()?;
        Self::from_mapping(mapping, Role::Child, false)
    }

    pub fn open_parent(os_id: &str) -> Result<Self, IpcError> {
        let mapping = ShmemConf::new().os_id(os_id).open()?;
        Self::from_mapping(mapping, Role::Parent, false)
    }

    pub fn os_id(&self) -> &str {
        self.mapping.get_os_id()
    }

    pub fn capacity(&self) -> usize {
        self.outbound.capacity
    }

    pub fn send(&mut self, message: &[u8], timeout: Duration) -> Result<(), IpcError> {
        if message.len() > self.outbound.capacity {
            return Err(IpcError::MessageTooLarge {
                len: message.len(),
                capacity: self.outbound.capacity,
            });
        }

        wait_event(&*self.outbound.empty_event, timeout)?;

        unsafe {
            ensure_state(&self.outbound, EMPTY)?;

            ptr::copy_nonoverlapping(message.as_ptr(), self.outbound.data, message.len());

            let control = &*self.outbound.control;

            control.len.store(message.len() as u32, Ordering::Relaxed);

            control.state.store(READY, Ordering::Release);
        }

        set_event(&*self.outbound.ready_event, EventState::Signaled)?;

        Ok(())
    }

    pub fn recv(&mut self, timeout: Duration) -> Result<Option<Vec<u8>>, IpcError> {
        if !wait_event_optional(&*self.inbound.ready_event, timeout) {
            return Ok(None);
        }

        self.read_inbound().map(Some)
    }

    pub fn recv_blocking(&mut self) -> Result<Vec<u8>, IpcError> {
        self.inbound
            .ready_event
            .wait(Timeout::Infinite)
            .map_err(|error| IpcError::Sync(error.to_string()))?;

        self.read_inbound()
    }

    /// Like `recv_blocking`, but exits with `Ok(None)` when `cancelled` was
    /// set before the wake. Pairs with [`Self::cancel_inbound_wait`] on
    /// another endpoint of the same role, so a reader thread parked on an
    /// infinite wait can be reclaimed at shutdown instead of leaking (each
    /// leaked reader also pins one shared-memory mapping).
    pub fn recv_blocking_cancellable(
        &mut self,
        cancelled: &AtomicBool,
    ) -> Result<Option<Vec<u8>>, IpcError> {
        self.inbound
            .ready_event
            .wait(Timeout::Infinite)
            .map_err(|error| IpcError::Sync(error.to_string()))?;

        if cancelled.load(Ordering::Acquire) {
            return Ok(None);
        }

        self.read_inbound().map(Some)
    }

    /// Signal the inbound ready event without publishing a message, to wake a
    /// thread blocked in [`Self::recv_blocking_cancellable`]. The cancelled
    /// flag must be set before calling this, or the woken reader will try to
    /// read a mailbox that holds nothing.
    pub fn cancel_inbound_wait(&self) -> Result<(), IpcError> {
        set_event(&*self.inbound.ready_event, EventState::Signaled)
    }

    fn read_inbound(&mut self) -> Result<Vec<u8>, IpcError> {
        unsafe {
            ensure_state(&self.inbound, READY)?;

            let control = &*self.inbound.control;
            let len = control.len.load(Ordering::Relaxed) as usize;

            if len > self.inbound.capacity {
                return Err(IpcError::CorruptLength {
                    len,
                    capacity: self.inbound.capacity,
                });
            }

            let mut message = vec![0; len];

            ptr::copy_nonoverlapping(self.inbound.data, message.as_mut_ptr(), len);

            control.state.store(EMPTY, Ordering::Release);

            set_event(&*self.inbound.empty_event, EventState::Signaled)?;

            Ok(message)
        }
    }

    fn from_mapping(mapping: Shmem, role: Role, create_events: bool) -> Result<Self, IpcError> {
        if mapping.len() < size_of::<MappingHeader>() {
            return Err(IpcError::InvalidLayout);
        }

        let base = mapping.as_ptr();

        if !(base as usize).is_multiple_of(align_of::<MappingHeader>()) {
            return Err(IpcError::InvalidLayout);
        }

        let header = unsafe { &*base.cast::<MappingHeader>() };

        if header.magic != MAGIC || header.version != VERSION {
            return Err(IpcError::ProtocolMismatch);
        }

        let capacity = header.capacity as usize;

        let layout = mapping_layout(capacity)?;

        // Windows reports an opened view rounded up to its allocation granularity,
        // while the creating process retains the requested length.
        if capacity == 0
            || header.event_size as usize != layout.event_size
            || layout.total > mapping.len()
        {
            return Err(IpcError::InvalidLayout);
        }

        let mut events = Vec::with_capacity(EVENT_COUNT);

        for index in 0..EVENT_COUNT {
            let event_ptr = unsafe { base.add(layout.events_offset + index * layout.event_size) };

            let (event, used) = unsafe {
                if create_events {
                    Event::new(event_ptr, true)
                } else {
                    Event::from_existing(event_ptr)
                }
            }
            .map_err(|error| IpcError::Sync(error.to_string()))?;

            if used != layout.event_size {
                return Err(IpcError::InvalidLayout);
            }

            events.push(event);
        }

        let mut events = events.into_iter();

        let first_data = unsafe { base.add(layout.data_offset) };
        let second_data = unsafe { first_data.add(capacity) };

        let parent_to_child = Mailbox {
            control: ptr::addr_of!(header.parent_to_child).cast_mut(),
            data: first_data,
            capacity,
            ready_event: events.next().expect("four events were created"),
            empty_event: events.next().expect("four events were created"),
        };

        let child_to_parent = Mailbox {
            control: ptr::addr_of!(header.child_to_parent).cast_mut(),
            data: second_data,
            capacity,
            ready_event: events.next().expect("four events were created"),
            empty_event: events.next().expect("four events were created"),
        };

        if create_events {
            set_event(&*parent_to_child.empty_event, EventState::Signaled)?;
            set_event(&*child_to_parent.empty_event, EventState::Signaled)?;
        }

        let (outbound, inbound) = match role {
            Role::Parent => (parent_to_child, child_to_parent),
            Role::Child => (child_to_parent, parent_to_child),
        };

        Ok(Self {
            mapping,
            outbound,
            inbound,
        })
    }
}

struct MappingLayout {
    event_size: usize,
    events_offset: usize,
    data_offset: usize,
    total: usize,
}

fn mapping_layout(capacity: usize) -> Result<MappingLayout, IpcError> {
    let event_size = Event::size_of(None);

    let events_offset = align_up(size_of::<MappingHeader>(), align_of::<u32>())?;

    let data_offset = event_size
        .checked_mul(EVENT_COUNT)
        .and_then(|events| events_offset.checked_add(events))
        .and_then(|offset| align_up(offset, 64).ok())
        .ok_or(IpcError::InvalidCapacity(capacity))?;

    let total = capacity
        .checked_mul(2)
        .and_then(|buffers| data_offset.checked_add(buffers))
        .ok_or(IpcError::InvalidCapacity(capacity))?;

    Ok(MappingLayout {
        event_size,
        events_offset,
        data_offset,
        total,
    })
}

fn align_up(value: usize, alignment: usize) -> Result<usize, IpcError> {
    value
        .checked_add(alignment - 1)
        .map(|value| value & !(alignment - 1))
        .ok_or(IpcError::InvalidLayout)
}

fn ensure_state(mailbox: &Mailbox, expected: u32) -> Result<(), IpcError> {
    let state = unsafe { &*mailbox.control }.state.load(Ordering::Acquire);

    if state == expected {
        Ok(())
    } else {
        Err(IpcError::CorruptState(state))
    }
}

fn wait_event(event: &dyn EventImpl, timeout: Duration) -> Result<(), IpcError> {
    event
        .wait(Timeout::Val(timeout))
        .map_err(|_| IpcError::Timeout)
}

fn wait_event_optional(event: &dyn EventImpl, timeout: Duration) -> bool {
    event.wait(Timeout::Val(timeout)).is_ok()
}

fn set_event(event: &dyn EventImpl, state: EventState) -> Result<(), IpcError> {
    event
        .set(state)
        .map_err(|error| IpcError::Sync(error.to_string()))
}

#[derive(Debug)]
pub enum IpcError {
    SharedMemory(ShmemError),
    InvalidCapacity(usize),
    InvalidLayout,
    ProtocolMismatch,
    MessageTooLarge { len: usize, capacity: usize },
    CorruptLength { len: usize, capacity: usize },
    CorruptState(u32),
    Sync(String),
    Timeout,
    MalformedMessage(&'static str),
    InvalidUtf8,
    Options(String),
}

impl fmt::Display for IpcError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SharedMemory(error) => error.fmt(formatter),
            Self::InvalidCapacity(capacity) => {
                write!(formatter, "invalid mailbox capacity {capacity}")
            }
            Self::InvalidLayout => write!(formatter, "invalid shared-memory layout"),
            Self::ProtocolMismatch => write!(formatter, "shared-memory protocol mismatch"),
            Self::MessageTooLarge { len, capacity } => {
                write!(
                    formatter,
                    "IPC message is {len} bytes; mailbox holds {capacity}"
                )
            }
            Self::CorruptLength { len, capacity } => {
                write!(
                    formatter,
                    "IPC message length {len} exceeds mailbox {capacity}"
                )
            }
            Self::CorruptState(state) => write!(formatter, "invalid mailbox state {state}"),
            Self::Sync(error) => write!(formatter, "shared-memory event failed: {error}"),
            Self::Timeout => write!(formatter, "shared-memory IPC timed out"),
            Self::MalformedMessage(message) => {
                write!(formatter, "malformed IPC message: {message}")
            }
            Self::InvalidUtf8 => write!(formatter, "IPC string is not UTF-8"),
            Self::Options(error) => write!(formatter, "invalid session options: {error}"),
        }
    }
}

impl error::Error for IpcError {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        match self {
            Self::SharedMemory(error) => Some(error),
            _ => None,
        }
    }
}

impl From<ShmemError> for IpcError {
    fn from(error: ShmemError) -> Self {
        Self::SharedMemory(error)
    }
}

#[derive(Debug)]
pub enum HubRequest {
    Open {
        request_id: u64,
        options: SessionOptions,
    },
    Input {
        session_id: SessionId,
        data: Vec<u8>,
    },
    Resize {
        session_id: SessionId,
        cols: u16,
        rows: u16,
    },
    Attach {
        request_id: u64,
        session_id: SessionId,
    },
    Detach {
        session_id: SessionId,
    },
    Kill {
        request_id: u64,
        session_id: SessionId,
    },
    ChildProcessCount {
        request_id: u64,
        session_id: SessionId,
    },
    Shutdown,
}

impl HubRequest {
    pub fn encode(&self) -> Result<Vec<u8>, IpcError> {
        let mut out = Vec::new();
        match self {
            Self::Open {
                request_id,
                options,
            } => {
                out.push(1);

                put_u64(&mut out, *request_id);

                out.extend_from_slice(
                    &to_vec(options).map_err(|error| IpcError::Options(error.to_string()))?,
                );
            }
            Self::Input { session_id, data } => {
                out.push(2);

                put_u64(&mut out, session_id.0);

                out.extend_from_slice(data);
            }
            Self::Resize {
                session_id,
                cols,
                rows,
            } => {
                out.push(3);

                put_u64(&mut out, session_id.0);
                put_u16(&mut out, *cols);
                put_u16(&mut out, *rows);
            }
            Self::Attach {
                request_id,
                session_id,
            } => {
                out.push(4);

                put_u64(&mut out, *request_id);
                put_u64(&mut out, session_id.0);
            }
            Self::Detach { session_id } => {
                out.push(5);

                put_u64(&mut out, session_id.0);
            }
            Self::Kill {
                request_id,
                session_id,
            } => {
                out.push(6);

                put_u64(&mut out, *request_id);
                put_u64(&mut out, session_id.0);
            }
            Self::Shutdown => out.push(7),
            Self::ChildProcessCount {
                request_id,
                session_id,
            } => {
                out.push(8);

                put_u64(&mut out, *request_id);
                put_u64(&mut out, session_id.0);
            }
        }
        Ok(out)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, IpcError> {
        let (&kind, mut payload) = bytes
            .split_first()
            .ok_or(IpcError::MalformedMessage("empty request"))?;

        let request = match kind {
            1 => {
                let request_id = take_u64(&mut payload)?;

                let options =
                    from_slice(payload).map_err(|error| IpcError::Options(error.to_string()))?;

                payload = &[];

                Self::Open {
                    request_id,
                    options,
                }
            }
            2 => Self::Input {
                session_id: SessionId(take_u64(&mut payload)?),
                data: mem::take(&mut payload).to_vec(),
            },
            3 => Self::Resize {
                session_id: SessionId(take_u64(&mut payload)?),
                cols: take_u16(&mut payload)?,
                rows: take_u16(&mut payload)?,
            },
            4 => Self::Attach {
                request_id: take_u64(&mut payload)?,
                session_id: SessionId(take_u64(&mut payload)?),
            },
            5 => Self::Detach {
                session_id: SessionId(take_u64(&mut payload)?),
            },
            6 => Self::Kill {
                request_id: take_u64(&mut payload)?,
                session_id: SessionId(take_u64(&mut payload)?),
            },
            7 => Self::Shutdown,
            8 => Self::ChildProcessCount {
                request_id: take_u64(&mut payload)?,
                session_id: SessionId(take_u64(&mut payload)?),
            },
            _ => return Err(IpcError::MalformedMessage("unknown request kind")),
        };

        if !payload.is_empty() {
            return Err(IpcError::MalformedMessage("trailing request bytes"));
        }

        Ok(request)
    }
}

#[derive(Debug)]
pub enum HubResponse {
    Opened {
        request_id: u64,
        session_id: SessionId,
    },
    Snapshot {
        request_id: u64,
        session_id: SessionId,
        base_seq: u64,
        cols: u16,
        rows: u16,
        vt: Vec<u8>,
    },
    Output {
        session_id: SessionId,
        seq: u64,
        data: Vec<u8>,
    },
    Exited {
        session_id: SessionId,
        seq: u64,
    },
    Ack {
        request_id: u64,
    },
    ChildProcessCount {
        request_id: u64,
        count: u64,
    },
    Error {
        request_id: u64,
        message: String,
    },
}

impl HubResponse {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        match self {
            Self::Opened {
                request_id,
                session_id,
            } => {
                out.push(0x81);

                put_u64(&mut out, *request_id);
                put_u64(&mut out, session_id.0);
            }
            Self::Snapshot {
                request_id,
                session_id,
                base_seq,
                cols,
                rows,
                vt,
            } => {
                out.push(0x82);

                put_u64(&mut out, *request_id);
                put_u64(&mut out, session_id.0);
                put_u64(&mut out, *base_seq);
                put_u16(&mut out, *cols);
                put_u16(&mut out, *rows);

                out.extend_from_slice(vt);
            }
            Self::Output {
                session_id,
                seq,
                data,
            } => {
                out.push(0x83);

                put_u64(&mut out, session_id.0);
                put_u64(&mut out, *seq);

                out.extend_from_slice(data);
            }
            Self::Exited { session_id, seq } => {
                out.push(0x84);

                put_u64(&mut out, session_id.0);
                put_u64(&mut out, *seq);
            }
            Self::Ack { request_id } => {
                out.push(0x85);

                put_u64(&mut out, *request_id);
            }
            Self::ChildProcessCount { request_id, count } => {
                out.push(0x86);

                put_u64(&mut out, *request_id);
                put_u64(&mut out, *count);
            }
            Self::Error {
                request_id,
                message,
            } => {
                out.push(0xff);

                put_u64(&mut out, *request_id);

                out.extend_from_slice(message.as_bytes());
            }
        }
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, IpcError> {
        let (&kind, mut payload) = bytes
            .split_first()
            .ok_or(IpcError::MalformedMessage("empty response"))?;

        let response = match kind {
            0x81 => Self::Opened {
                request_id: take_u64(&mut payload)?,
                session_id: SessionId(take_u64(&mut payload)?),
            },
            0x82 => Self::Snapshot {
                request_id: take_u64(&mut payload)?,
                session_id: SessionId(take_u64(&mut payload)?),
                base_seq: take_u64(&mut payload)?,
                cols: take_u16(&mut payload)?,
                rows: take_u16(&mut payload)?,
                vt: mem::take(&mut payload).to_vec(),
            },
            0x83 => Self::Output {
                session_id: SessionId(take_u64(&mut payload)?),
                seq: take_u64(&mut payload)?,
                data: mem::take(&mut payload).to_vec(),
            },
            0x84 => Self::Exited {
                session_id: SessionId(take_u64(&mut payload)?),
                seq: take_u64(&mut payload)?,
            },
            0x85 => Self::Ack {
                request_id: take_u64(&mut payload)?,
            },
            0x86 => Self::ChildProcessCount {
                request_id: take_u64(&mut payload)?,
                count: take_u64(&mut payload)?,
            },
            0xff => Self::Error {
                request_id: take_u64(&mut payload)?,
                message: str::from_utf8(mem::take(&mut payload))
                    .map_err(|_| IpcError::InvalidUtf8)?
                    .to_owned(),
            },
            _ => return Err(IpcError::MalformedMessage("unknown response kind")),
        };

        if !payload.is_empty() {
            return Err(IpcError::MalformedMessage("trailing response bytes"));
        }

        Ok(response)
    }
}

fn put_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn take_u16(bytes: &mut &[u8]) -> Result<u16, IpcError> {
    Ok(u16::from_le_bytes(take(bytes)?))
}

fn take_u64(bytes: &mut &[u8]) -> Result<u64, IpcError> {
    Ok(u64::from_le_bytes(take(bytes)?))
}

fn take<const N: usize>(bytes: &mut &[u8]) -> Result<[u8; N], IpcError> {
    let (value, rest) = bytes
        .split_first_chunk::<N>()
        .ok_or(IpcError::MalformedMessage("truncated integer"))?;

    *bytes = rest;

    Ok(*value)
}

pub fn run_hub_host(os_id: &str) -> Result<(), IpcError> {
    let mut endpoint = SharedMemoryEndpoint::open_child(os_id)?;

    let hub = RemoteSessionHub::new();

    let mut subscriptions = HashMap::<SessionId, SessionSubscription>::new();

    let (request_sender, request_receiver) = sync::mpsc::channel();
    let host_thread = thread::current();
    let reader_wake = host_thread.clone();
    let reader_os_id = os_id.to_owned();

    thread::Builder::new()
        .name("remote-session-hub-ipc-reader".to_owned())
        .spawn(move || {
            let result = (|| -> Result<(), IpcError> {
                let mut reader = SharedMemoryEndpoint::open_child(&reader_os_id)?;

                loop {
                    let message = reader.recv_blocking()?;

                    if request_sender.send(Ok(message)).is_err() {
                        return Ok(());
                    }

                    reader_wake.unpark();
                }
            })();

            if let Err(error) = result {
                let _ = request_sender.send(Err(error.to_string()));
                reader_wake.unpark();
            }
        })
        .map_err(|error| IpcError::Sync(error.to_string()))?;

    loop {
        let mut did_work = false;

        while let Ok(message) = request_receiver.try_recv() {
            did_work = true;

            let message = message.map_err(IpcError::Sync)?;

            let request = match HubRequest::decode(&message) {
                Ok(request) => request,
                Err(error) => {
                    send_response(
                        &mut endpoint,
                        HubResponse::Error {
                            request_id: 0,
                            message: error.to_string(),
                        },
                    )?;

                    continue;
                }
            };
            match request {
                HubRequest::Open {
                    request_id,
                    options,
                } => match hub.open(options) {
                    Ok(session_id) => {
                        send_response(
                            &mut endpoint,
                            HubResponse::Opened {
                                request_id,
                                session_id,
                            },
                        )?;
                    }
                    Err(error) => {
                        send_error(&mut endpoint, request_id, error)?;
                    }
                },
                HubRequest::Input { session_id, data } => {
                    if let Err(error) = hub.write_input(session_id, &data) {
                        send_error(&mut endpoint, 0, error)?;
                    }
                }
                HubRequest::Resize {
                    session_id,
                    cols,
                    rows,
                } => {
                    if let Err(error) = hub.resize(session_id, cols, rows) {
                        send_error(&mut endpoint, 0, error)?;
                    }
                }
                HubRequest::Attach {
                    request_id,
                    session_id,
                } => match hub.attach(session_id) {
                    Ok(subscription) => {
                        subscription.set_wake_thread(host_thread.clone());

                        let snapshot = subscription.snapshot();

                        // Register the subscription only when the snapshot was
                        // delivered — a client that never saw the checkpoint
                        // cannot consume the event stream that follows it.
                        if send_response(
                            &mut endpoint,
                            HubResponse::Snapshot {
                                request_id,
                                session_id,
                                base_seq: snapshot.base_seq,
                                cols: snapshot.cols,
                                rows: snapshot.rows,
                                vt: snapshot.vt.clone(),
                            },
                        )? {
                            subscriptions.insert(session_id, subscription);
                        }
                    }
                    Err(error) => {
                        send_error(&mut endpoint, request_id, error)?;
                    }
                },
                HubRequest::Detach { session_id } => {
                    subscriptions.remove(&session_id);
                }
                HubRequest::Kill {
                    request_id,
                    session_id,
                } => match hub.kill(session_id) {
                    Ok(()) => {
                        send_response(&mut endpoint, HubResponse::Ack { request_id })?;
                    }
                    Err(error) => {
                        send_error(&mut endpoint, request_id, error)?;
                    }
                },
                HubRequest::ChildProcessCount {
                    request_id,
                    session_id,
                } => match hub.child_process_count(session_id) {
                    Ok(count) => {
                        send_response(
                            &mut endpoint,
                            HubResponse::ChildProcessCount {
                                request_id,
                                count: count as u64,
                            },
                        )?;
                    }
                    Err(error) => {
                        send_error(&mut endpoint, request_id, error)?;
                    }
                },
                HubRequest::Shutdown => return Ok(()),
            }
        }

        let mut events = Vec::new();

        // Scanning every subscription and fully draining each queue keeps dispatch
        // simple, but many concurrently busy sessions can delay colder ones. If load
        // tests expose cross-session latency or outbound-mailbox head-of-line blocking,
        // use a ready-session queue with a per-session byte budget here.
        subscriptions.retain(|session_id, subscription| {
            let mut keep = true;

            while let Ok(event) = subscription.events().try_recv() {
                match event {
                    SessionEvent::Output { seq, data } => events.push(HubResponse::Output {
                        session_id: *session_id,
                        seq,
                        data: data.to_vec(),
                    }),
                    SessionEvent::Exited { seq } => {
                        events.push(HubResponse::Exited {
                            session_id: *session_id,
                            seq,
                        });
                        keep = false;
                        break;
                    }
                }
            }
            keep
        });

        did_work |= !events.is_empty();

        for event in events {
            if !send_response(&mut endpoint, event)? {
                // The client is not draining its mailbox: drop every
                // subscription so a wedged client costs one timeout, not one
                // per queued event. Sessions stay alive; the client's
                // sequence-gap detection makes it re-attach when it recovers.
                subscriptions.clear();
                break;
            }
        }

        if !did_work {
            thread::park();
        }
    }
}

fn send_error(
    endpoint: &mut SharedMemoryEndpoint,
    request_id: u64,
    error: impl fmt::Display,
) -> Result<bool, IpcError> {
    send_response(
        endpoint,
        HubResponse::Error {
            request_id,
            message: error.to_string(),
        },
    )
}

/// Send a response, treating a full client mailbox as message loss instead of
/// a host failure. Returns `Ok(false)` when the client did not drain its
/// mailbox within `HOST_SEND_TIMEOUT`: the hub's core promise is that
/// sessions survive a detached (or wedged) client, so a stalled client must
/// never propagate as an error that tears down `run_hub_host` and with it
/// every ConPTY session. The client recovers on its own: a dropped Output
/// creates a sequence gap, which forces it to re-attach from a fresh
/// checkpoint. Non-timeout errors still surface — they mean the transport
/// itself is broken.
fn send_response(
    endpoint: &mut SharedMemoryEndpoint,
    response: HubResponse,
) -> Result<bool, IpcError> {
    match endpoint.send(&response.encode(), HOST_SEND_TIMEOUT) {
        Ok(()) => Ok(true),
        Err(IpcError::Timeout) => {
            eprintln!("SessionHub: client mailbox stalled; dropping a response");
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use std::time;

    use super::*;

    #[test]
    fn shared_mailboxes_preserve_both_directions_and_message_boundaries() {
        let mut parent = SharedMemoryEndpoint::create_parent(64).unwrap();
        let mut child = SharedMemoryEndpoint::open_child(parent.os_id()).unwrap();

        parent.send(b"input", Duration::ZERO).unwrap();
        assert_eq!(child.recv(Duration::ZERO).unwrap().unwrap(), b"input");

        child.send(b"output", Duration::ZERO).unwrap();
        assert_eq!(parent.recv(Duration::ZERO).unwrap().unwrap(), b"output");
        assert!(parent.recv(Duration::ZERO).unwrap().is_none());
    }

    #[test]
    fn blocking_receive_is_released_by_the_cross_process_event() {
        let mut parent = SharedMemoryEndpoint::create_parent(64).unwrap();
        let os_id = parent.os_id().to_owned();
        let sender = thread::spawn(move || {
            let mut child = SharedMemoryEndpoint::open_child(&os_id).unwrap();
            thread::sleep(Duration::from_millis(25));
            child.send(b"wake", Duration::from_secs(1)).unwrap();
        });

        let started = time::Instant::now();
        assert_eq!(parent.recv_blocking().unwrap(), b"wake");
        assert!(started.elapsed() >= Duration::from_millis(20));
        sender.join().unwrap();
    }

    #[test]
    fn binary_messages_round_trip_without_encoding_terminal_bytes_as_json() {
        let request = HubRequest::Input {
            session_id: SessionId(7),
            data: vec![0, 0xff, b'\r'],
        };
        let HubRequest::Input { session_id, data } =
            HubRequest::decode(&request.encode().unwrap()).unwrap()
        else {
            panic!("wrong request kind");
        };
        assert_eq!(session_id, SessionId(7));
        assert_eq!(data, [0, 0xff, b'\r']);
    }
}
