#[cfg(test)]
#[path = "library_tests.rs"]
mod library_tests;

use std::ffi::{CStr, c_void};
use std::io;
use std::path::Path;
use std::ptr::NonNull;

#[cfg(unix)]
use crate::unix::library::{load, symbol};
#[cfg(windows)]
use crate::windows::library::{load, symbol};

/// An untyped function address. Calling it requires conversion to the export's
/// actual signature and adherence to that function's safety requirements.
pub type LibrarySymbol = unsafe extern "system" fn() -> isize;

/// A library retained until process exit, even after this value is dropped.
/// Parser tables and callbacks may retain its addresses beyond registration,
/// so no destructor unloads the module or invalidates previously found symbols.
pub struct ResidentLibrary {
    handle: NonNull<c_void>,
}

impl ResidentLibrary {
    /// # Safety
    /// The library and its dependencies must be trusted to execute their
    /// initialization routines in the current process.
    pub unsafe fn load(path: &Path) -> io::Result<Self> {
        // SAFETY: the caller accepts execution of the library's initializers.
        let handle = unsafe { load(path) }?;

        Ok(Self { handle })
    }

    pub fn symbol(&self, name: &CStr) -> Option<LibrarySymbol> {
        // SAFETY: only a successful load creates this handle, and it is never
        // unloaded. Looking up an address does not invoke the export.
        unsafe { symbol(self.handle, name) }
    }
}
