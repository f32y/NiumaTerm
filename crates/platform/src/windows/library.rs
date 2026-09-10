use std::ffi::{CStr, c_void};
use std::io;
use std::os::windows::ffi::OsStrExt as _;
use std::path::Path;
use std::ptr::NonNull;

use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

use crate::library::LibrarySymbol;

pub(crate) unsafe fn load(path: &Path) -> io::Result<NonNull<c_void>> {
    let mut name: Vec<u16> = path.as_os_str().encode_wide().collect();
    if name.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "library path contains a NUL",
        ));
    }
    name.push(0);
    // SAFETY: the caller accepts the module's initializers; the terminated
    // path buffer remains valid throughout the native call.
    NonNull::new(unsafe { LoadLibraryW(name.as_ptr()) }).ok_or_else(io::Error::last_os_error)
}

pub(crate) unsafe fn symbol(handle: NonNull<c_void>, name: &CStr) -> Option<LibrarySymbol> {
    // SAFETY: the caller provides a loaded, resident handle and a terminated name.
    unsafe { GetProcAddress(handle.as_ptr(), name.as_ptr().cast()) }
}
