use std::{mem, os, ptr, slice, str, sync};

use image_rs::load_from_memory;
use libghostty_vt_sys::{
    Allocator as VtAllocator, ClipboardLocation as VtClipboardLocation,
    ClipboardWrite as VtClipboardWrite, ClipboardWriteResult as VtClipboardWriteResult,
    String as VtString, SysImage as VtSysImage, SysOption as VtSysOption, Terminal as VtTerminal,
    ghostty_alloc, ghostty_sys_set,
};

use crate::clipboard;

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
}

pub(super) unsafe extern "C" fn write_pty_cb(
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

pub(super) unsafe extern "C" fn bell_cb(_terminal: VtTerminal, userdata: *mut os::raw::c_void) {
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

pub(super) unsafe extern "C" fn clipboard_write_cb(
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
