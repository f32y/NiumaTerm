use std::ffi::{CStr, CString, c_void};
use std::os::unix::ffi::OsStrExt as _;
use std::path::Path;
use std::ptr::NonNull;
use std::{io, mem};

use libc::{RTLD_LAZY, RTLD_LOCAL, dlerror, dlopen, dlsym};

use crate::library::LibrarySymbol;

pub(crate) unsafe fn load(path: &Path) -> io::Result<NonNull<c_void>> {
    let name = CString::new(path.as_os_str().as_bytes())
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;

    // RTLD_LOCAL keeps symbols out of later global lookups. The caller accepts
    // initializers, and the terminated path remains valid throughout the call.
    NonNull::new(unsafe { dlopen(name.as_ptr(), RTLD_LAZY | RTLD_LOCAL) }).ok_or_else(|| {
        // dlopen reports through dlerror, not errno. Copy before another loader
        // operation on this thread can replace the diagnostic buffer.
        let error = unsafe { dlerror() };

        let message = if error.is_null() {
            "unknown library loading error".into()
        } else {
            unsafe { CStr::from_ptr(error) }
                .to_string_lossy()
                .into_owned()
        };

        io::Error::other(message)
    })
}

pub(crate) unsafe fn symbol(handle: NonNull<c_void>, name: &CStr) -> Option<LibrarySymbol> {
    // SAFETY: the caller provides a loaded, resident handle and a terminated name.
    let address = unsafe { dlsym(handle.as_ptr(), name.as_ptr()) };

    (!address.is_null()).then(|| {
        // POSIX permits converting a dlsym address to a function pointer. The
        // consumer must choose the export's actual signature before calling it.
        unsafe { mem::transmute::<*mut c_void, LibrarySymbol>(address) }
    })
}
