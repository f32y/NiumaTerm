use std::num::NonZeroIsize;

use std::ptr;

use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use windows_sys::Win32::Foundation::HWND;

use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, IsIconic, MB_ICONERROR, MB_OK, MessageBoxW,
};

pub fn is_foreground_and_not_minimized(hwnd: NonZeroIsize) -> bool {
    let hwnd = hwnd.get() as HWND;

    unsafe { GetForegroundWindow() == hwnd && IsIconic(hwnd) == 0 }
}

pub fn show_error_dialog(title: &str, message: &str) {
    let title: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
    let message: Vec<u16> = message.encode_utf16().chain(Some(0)).collect();

    unsafe {
        MessageBoxW(
            ptr::null_mut(),
            message.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}

/// Query native activation because the initial show can precede GPUI's
/// activation event and leave its cached value unset.
pub fn native_active_state(window: &impl HasWindowHandle) -> Option<bool> {
    let Ok(handle) = window.window_handle() else {
        return Some(false);
    };

    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return Some(false);
    };

    Some(is_foreground_and_not_minimized(handle.hwnd))
}
