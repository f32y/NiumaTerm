/// The `mio` types this crate's `ProcessReadWrite`/`EventedPty` surface is built
/// on, re-exported so consumers drive the PTY event loop through
/// `nmt_platform::{Poll, ...}` without taking their own (possibly mismatched)
/// `mio` dependency.
use std::path;
use std::{sync, thread};

use libc::c_ushort;
pub use mio::{Events, Interest, Poll, Token, Waker};

#[cfg(not(windows))]
mod unix;
#[cfg(not(windows))]
use crate::unix as platform;
#[cfg(not(windows))]
pub use crate::unix::*;

#[cfg(windows)]
pub mod windows;
use std::io;

#[cfg(windows)]
use crate::windows as platform;
#[cfg(windows)]
pub use crate::windows::*;

pub const APP_ID: &str = "NiumaTerm";

/// Process-wide toggle: manage spawned shells with a Windows Job Object
/// (`KILL_ON_JOB_CLOSE`), so closing a tab kills the shell's entire process
/// tree. Read at spawn time — only affects PTYs created afterwards. No-op on
/// non-Windows platforms.
static JOB_MANAGEMENT: sync::atomic::AtomicBool = sync::atomic::AtomicBool::new(false);

pub fn set_job_management(enabled: bool) {
    JOB_MANAGEMENT.store(enabled, sync::atomic::Ordering::Relaxed);
}

pub fn job_management_enabled() -> bool {
    JOB_MANAGEMENT.load(sync::atomic::Ordering::Relaxed)
}

#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn job_management() -> bool {
    job_management_enabled()
}

#[repr(C)]
pub struct Winsize {
    ws_row: c_ushort,
    ws_col: c_ushort,
    ws_xpixel: c_ushort,
    ws_ypixel: c_ushort,
}

pub trait ProcessReadWrite {
    type Reader: io::Read;
    type Writer: io::Write;
    fn reader(&mut self) -> &mut Self::Reader;
    fn read_token(&self) -> Token;
    fn writer(&mut self) -> &mut Self::Writer;
    fn write_token(&self) -> Token;
    fn set_winsize(&mut self, _: WinsizeBuilder) -> Result<(), io::Error>;

    /// Register the PTY's sources with the event loop's `Poll`, pulling tokens from
    /// the iterator. `waker` is the loop's `mio::Waker`: the Windows ConPTY worker
    /// threads have no real OS readiness source, so they signal "data ready" through
    /// this waker. The Unix path registers real fds and ignores the waker.
    fn register(
        &mut self,
        _: &Poll,
        _: &mut dyn Iterator<Item = Token>,
        _: Interest,
        _: &sync::Arc<Waker>,
    ) -> io::Result<()>;
    fn reregister(&mut self, _: &Poll, _: Interest) -> io::Result<()>;
    fn deregister(&mut self, _: &Poll) -> io::Result<()>;

    /// Tokens whose soft-ready flag is currently set (Windows ConPTY worker-thread
    /// readiness). The Unix path has real OS readiness and returns an empty iterator.
    /// The event loop feeds these through the same `match token` arms it uses for
    /// real `Poll` events.
    fn drain_ready(&self) -> Vec<Token>;

    /// Whether any soft-ready flag is currently set (Windows ConPTY level readiness),
    /// without allocating or clearing it. The event loop checks this before blocking
    /// in `poll()`: a `pty_read` capped by `MAX_LOCKED_READ` can return with data still
    /// in the ring (flag left set), and the worker only wakes on the clear→set edge, so
    /// a blocking `poll(None)` would sleep forever on already-signalled data. The Unix
    /// path has real OS readiness (re-armed by `EPOLL_CTL_MOD`) and returns `false`.
    fn has_ready(&self) -> bool {
        false
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ChildEvent {
    Exited,
}

pub trait EventedPty: ProcessReadWrite {
    fn child_event_token(&self) -> Token;

    /// Tries to retrieve an event.
    ///
    /// Returns `Some(event)` on success, or `None` if there are no events to retrieve.
    fn next_child_event(&mut self) -> Option<ChildEvent>;
}

#[derive(Debug, Clone)]
pub struct WinsizeBuilder {
    pub rows: u16,
    pub cols: u16,
    pub width: u16,
    pub height: u16,
}

impl WinsizeBuilder {
    fn build(&self) -> Winsize {
        let ws_row = self.rows as c_ushort;
        let ws_col = self.cols as c_ushort;
        let ws_xpixel = self.width as c_ushort;
        let ws_ypixel = self.height as c_ushort;

        Winsize {
            ws_row,
            ws_col,
            ws_xpixel,
            ws_ypixel,
        }
    }
}

/// Request notification authorization from the OS.
/// On macOS this triggers the permission prompt on first call.
/// No-op on other platforms.
pub fn request_authorization() {
    #[cfg(target_os = "macos")]
    platform::request_authorization();
}

/// Send a desktop notification using the platform's native API.
///
/// Spawns a background thread so the caller is never blocked.
pub fn send_notification(title: &str, body: &str) {
    let title = if title.is_empty() {
        APP_ID.to_string()
    } else {
        title.to_string()
    };

    let body = body.to_string();

    thread::spawn(move || {
        let _ = platform::show(&NativeNotification {
            title,
            body,
            activation_url: String::new(),
            tag: String::new(),
            group: String::new(),
        });
    });
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeNotification {
    pub title: String,
    pub body: String,
    pub activation_url: String,
    pub tag: String,
    pub group: String,
}

pub fn show_notification(notification: &NativeNotification) -> Result<(), String> {
    platform::show(notification)
}

pub fn remove_notification(tag: &str, group: &str) -> Result<(), String> {
    platform::remove(tag, group)
}

pub fn register_application_identity(exe_path: &path::Path) -> Result<(), String> {
    platform::register_identity(exe_path)
}

pub fn unregister_application_identity() -> Result<(), String> {
    platform::unregister_identity()
}

pub fn application_identity_registered() -> bool {
    platform::identity_registered()
}
