use std::{mem, os, ptr, slice, str, sync};

use image_rs::load_from_memory;
use libghostty_vt_sys::{
    Allocator as VtAllocator, ClipboardLocation as VtClipboardLocation,
    ClipboardWrite as VtClipboardWrite, ClipboardWriteResult as VtClipboardWriteResult,
    String as VtString, SysImage as VtSysImage, SysOption as VtSysOption, Terminal as VtTerminal,
    TerminalDesktopNotification as VtDesktopNotification, TerminalOption as VtTerminalOption,
    TerminalProgressReport as VtProgressReport, TerminalProgressState as VtProgressState,
    ghostty_alloc, ghostty_sys_set, ghostty_terminal_set,
};

use crate::clipboard;
use crate::event::{ProgressReport, ProgressState};

/// State the terminal's synchronous callbacks write into during `write_vt`.
/// Owned behind a `Box` so its address is stable for the FFI userdata pointer.
#[derive(Default)]
pub(super) struct Callbacks {
    /// Bytes the terminal wants written back to the PTY (DSR/DA/etc.).
    pub(super) pty_writes: Vec<u8>,

    /// Number of BEL characters received since last drained.
    pub(super) bell_count: u32,

    /// Owned text copied from clipboard requests before the FFI callback returns.
    pub(super) clipboard_writes: Vec<(clipboard::ClipboardType, String)>,

    /// The latest OSC 9;4 report since last drained. Later reports in one
    /// batch supersede earlier ones because the tab strip only shows the
    /// current state.
    pub(super) progress: Option<ProgressReport>,

    /// A progress indicator is showing (last report was anything but
    /// `Remove`). Published frames hide their cursor while this holds, and
    /// the engine's DECTCEM state stays untouched so removal restores the
    /// exact cursor the program left.
    pub(super) progress_active: bool,

    /// OSC 9 / OSC 777 desktop notifications as `(title, body)`, copied out
    /// before the callback returns because the engine only lends the strings.
    pub(super) notifications: Vec<(String, String)>,

    /// The title changed (OSC 0/2) since the flag was last taken. The engine
    /// keeps the value itself, so only the edge is recorded here and the
    /// string is read once when the change is reported.
    pub(super) title_changed: bool,

    /// The working directory changed (OSC 7/9/1337) since the flag was last
    /// taken; the value is read from the engine the same way as the title.
    pub(super) pwd_changed: bool,
}

/// Register the terminal's synchronous callbacks, which write into the
/// returned `Callbacks`. The terminal keeps the box's heap address as its
/// userdata, which stays put when the box moves.
///
/// # Safety
///
/// `terminal` must be a live terminal handle, and the returned box must
/// outlive it: every later `write_vt` can call back through the pointer.
pub(super) unsafe fn install_callbacks(terminal: VtTerminal) -> Box<Callbacks> {
    let mut callbacks = Box::new(Callbacks::default());

    let userdata = &mut *callbacks as *mut Callbacks as *mut os::raw::c_void;

    unsafe {
        ghostty_terminal_set(terminal, VtTerminalOption::USERDATA, userdata);

        ghostty_terminal_set(
            terminal,
            VtTerminalOption::WRITE_PTY,
            write_pty_cb as *const os::raw::c_void,
        );

        ghostty_terminal_set(
            terminal,
            VtTerminalOption::BELL,
            bell_cb as *const os::raw::c_void,
        );

        ghostty_terminal_set(
            terminal,
            VtTerminalOption::CLIPBOARD_WRITE,
            clipboard_write_cb as *const os::raw::c_void,
        );

        ghostty_terminal_set(
            terminal,
            VtTerminalOption::PROGRESS_REPORT,
            progress_report_cb as *const os::raw::c_void,
        );

        ghostty_terminal_set(
            terminal,
            VtTerminalOption::DESKTOP_NOTIFICATION,
            desktop_notification_cb as *const os::raw::c_void,
        );

        ghostty_terminal_set(
            terminal,
            VtTerminalOption::TITLE_CHANGED,
            title_changed_cb as *const os::raw::c_void,
        );

        ghostty_terminal_set(
            terminal,
            VtTerminalOption::PWD_CHANGED,
            pwd_changed_cb as *const os::raw::c_void,
        );
    }

    callbacks
}

unsafe extern "C" fn title_changed_cb(_terminal: VtTerminal, userdata: *mut os::raw::c_void) {
    if userdata.is_null() {
        return;
    }

    let cb = unsafe { &mut *(userdata as *mut Callbacks) };

    cb.title_changed = true;
}

unsafe extern "C" fn pwd_changed_cb(_terminal: VtTerminal, userdata: *mut os::raw::c_void) {
    if userdata.is_null() {
        return;
    }

    let cb = unsafe { &mut *(userdata as *mut Callbacks) };

    cb.pwd_changed = true;
}

unsafe extern "C" fn progress_report_cb(
    _terminal: VtTerminal,
    userdata: *mut os::raw::c_void,
    report: *const VtProgressReport,
) {
    if userdata.is_null() || report.is_null() {
        return;
    }

    let size = unsafe { report.cast::<usize>().read() };

    if size < mem::size_of::<VtProgressReport>() {
        return;
    }

    let report = unsafe { &*report };

    let state = match report.state {
        VtProgressState::REMOVE => ProgressState::Remove,
        VtProgressState::SET => ProgressState::Set,
        VtProgressState::ERROR => ProgressState::Error,
        VtProgressState::INDETERMINATE => ProgressState::Indeterminate,
        VtProgressState::PAUSE => ProgressState::Pause,
        _ => return,
    };

    // `-1` marks an omitted percentage (PowerShell ends its indicator with
    // `ESC ] 9 ; 4 ; 0 ST`). Values past 100 clamp rather than drop so a
    // miscounting emitter still gets a full bar.
    let progress = u8::try_from(report.progress)
        .ok()
        .map(|value| value.min(100));

    let cb = unsafe { &mut *(userdata as *mut Callbacks) };

    cb.progress_active = state != ProgressState::Remove;
    cb.progress = Some(ProgressReport { state, progress });
}

unsafe extern "C" fn desktop_notification_cb(
    _terminal: VtTerminal,
    userdata: *mut os::raw::c_void,
    notification: *const VtDesktopNotification,
) {
    if userdata.is_null() || notification.is_null() {
        return;
    }

    // Sized struct: an older engine may hand over fewer fields than this
    // binding knows about.
    let size = unsafe { notification.cast::<usize>().read() };

    if size < mem::size_of::<VtDesktopNotification>() {
        return;
    }

    let notification = unsafe { &*notification };

    let text = |value: &VtString| {
        unsafe { vt_string_bytes(value) }.map(|bytes| String::from_utf8_lossy(bytes).into_owned())
    };

    let (Some(title), Some(body)) = (text(&notification.title), text(&notification.body)) else {
        return;
    };

    let cb = unsafe { &mut *(userdata as *mut Callbacks) };

    cb.notifications.push((title, body));
}

unsafe extern "C" fn write_pty_cb(
    _terminal: VtTerminal,
    userdata: *mut os::raw::c_void,
    data: *const u8,
    len: usize,
) {
    if userdata.is_null() || data.is_null() || len == 0 {
        return;
    }

    let cb = unsafe { &mut *(userdata as *mut Callbacks) };

    cb.pty_writes
        .extend_from_slice(unsafe { slice::from_raw_parts(data, len) });
}

unsafe extern "C" fn bell_cb(_terminal: VtTerminal, userdata: *mut os::raw::c_void) {
    if userdata.is_null() {
        return;
    }

    let cb = unsafe { &mut *(userdata as *mut Callbacks) };

    cb.bell_count = cb.bell_count.saturating_add(1);
}

unsafe fn vt_string_bytes(value: &VtString) -> Option<&[u8]> {
    if value.len == 0 {
        return Some(&[]);
    }

    if value.ptr.is_null() {
        return None;
    }

    Some(unsafe { slice::from_raw_parts(value.ptr, value.len) })
}

unsafe extern "C" fn clipboard_write_cb(
    _terminal: VtTerminal,
    userdata: *mut os::raw::c_void,
    write: *const VtClipboardWrite,
) -> VtClipboardWriteResult::Type {
    use crate::clipboard::ClipboardType;

    if userdata.is_null() || write.is_null() {
        return VtClipboardWriteResult::INVALID_DATA;
    }

    let size = unsafe { write.cast::<usize>().read() };

    if size < mem::size_of::<VtClipboardWrite>() {
        return VtClipboardWriteResult::INVALID_DATA;
    }

    let write = unsafe { &*write };

    let ty = match write.location {
        VtClipboardLocation::STANDARD => ClipboardType::Clipboard,
        VtClipboardLocation::SELECTION | VtClipboardLocation::PRIMARY => ClipboardType::Selection,
        _ => return VtClipboardWriteResult::UNSUPPORTED,
    };

    let cb = unsafe { &mut *(userdata as *mut Callbacks) };

    if write.contents_len == 0 {
        cb.clipboard_writes.push((ty, String::new()));

        return VtClipboardWriteResult::SUCCESS;
    }

    if write.contents.is_null() {
        return VtClipboardWriteResult::INVALID_DATA;
    }

    let contents = unsafe { slice::from_raw_parts(write.contents, write.contents_len) };

    for content in contents {
        let Some(mime) = (unsafe { vt_string_bytes(&content.mime) }) else {
            return VtClipboardWriteResult::INVALID_DATA;
        };

        if mime != b"text/plain" && !mime.starts_with(b"text/plain;") {
            continue;
        }

        let Some(data) = (unsafe { vt_string_bytes(&content.data) }) else {
            return VtClipboardWriteResult::INVALID_DATA;
        };

        let Ok(text) = str::from_utf8(data) else {
            return VtClipboardWriteResult::INVALID_DATA;
        };

        cb.clipboard_writes.push((ty, text.to_owned()));

        return VtClipboardWriteResult::SUCCESS;
    }

    VtClipboardWriteResult::UNSUPPORTED
}

/// PNG decode hook for the engine's kitty graphics protocol. The
/// `.lib` artifact ships no PNG decoder, so without this `f=100` transmissions are
/// rejected. Decodes via `image_rs` to RGBA and returns the buffer allocated with
/// the engine's own allocator (so the engine frees it).
unsafe extern "C" fn decode_png_cb(
    _userdata: *mut os::raw::c_void,
    allocator: *const VtAllocator,
    data: *const u8,
    data_len: usize,
    out: *mut VtSysImage,
) -> bool {
    if data.is_null() || out.is_null() {
        return false;
    }

    let bytes = unsafe { slice::from_raw_parts(data, data_len) };

    let img = match load_from_memory(bytes) {
        Ok(img) => img.to_rgba8(),
        Err(_) => return false,
    };

    let (w, h) = (img.width(), img.height());

    let rgba = img.into_raw();

    let buf = unsafe { ghostty_alloc(allocator, rgba.len()) };

    if buf.is_null() {
        return false;
    }

    unsafe {
        ptr::copy_nonoverlapping(rgba.as_ptr(), buf, rgba.len());

        (*out).width = w;
        (*out).height = h;
        (*out).data = buf;
        (*out).data_len = rgba.len();
    }

    true
}

/// Register the process-global PNG decode hook once.
pub(super) fn register_png_decoder() {
    static ONCE: sync::Once = sync::Once::new();

    ONCE.call_once(|| unsafe {
        ghostty_sys_set(
            VtSysOption::GHOSTTY_SYS_OPT_DECODE_PNG,
            decode_png_cb as *const os::raw::c_void,
        );
    });
}

/// Kitty image storage limit. The `.lib` default is 10 MB — small
/// enough to evict real images; 64 MB holds typical multi-image use with a bounded
/// resident footprint (~2–3× at saturation). Future `graphics` config knob.
pub(super) const KITTY_IMAGE_STORAGE_LIMIT_BYTES: u64 = 64 * 1024 * 1024;
