use std::ffi::c_void;
use std::{io, mem, ptr};

use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Threading::{
    INFINITE, RegisterWaitForSingleObject, UnregisterWaitEx,
};

/// A wait-thread callback registered on a kernel object, together with the
/// context it borrows. The context is boxed so the address handed to the
/// kernel stays valid while this value moves, and it is freed only after a
/// blocking unregister has excluded any callback still running.
pub(super) struct RegisteredWait<T> {
    wait: HANDLE,
    context: Box<T>,
}

// The wait handle is an opaque kernel token, and the callback reaches the
// context from the wait thread, which is why `T` must be `Sync`.
unsafe impl<T: Send + Sync> Send for RegisteredWait<T> {}

impl<T> RegisteredWait<T> {
    /// Register `callback` to run with a pointer to `context` whenever
    /// `object` is signalled. `flags` are the `WT_*` wait options.
    pub(super) fn new(
        object: HANDLE,
        context: T,
        callback: unsafe extern "system" fn(*mut c_void, bool),
        flags: u32,
    ) -> io::Result<Self>
    where
        T: Sync,
    {
        let context = Box::new(context);

        let mut wait: HANDLE = ptr::null_mut();

        let registered = unsafe {
            RegisterWaitForSingleObject(
                &mut wait,
                object,
                Some(callback),
                ptr::from_ref::<T>(&context).cast_mut().cast(),
                INFINITE,
                flags,
            )
        };

        if registered == 0 {
            return Err(io::Error::last_os_error());
        }

        Ok(Self { wait, context })
    }

    pub(super) fn context(&self) -> &T {
        &self.context
    }

    /// Stop the callback. Returns only after any callback still running has
    /// finished, which is what makes freeing `context` afterwards sound. It
    /// never runs on the wait-callback thread, so it cannot self-deadlock.
    /// Dropping does the same; calling it earlier lets the owner take over
    /// the object the callback was watching.
    pub(super) fn unregister(&mut self) {
        let wait = mem::replace(&mut self.wait, ptr::null_mut());

        if !wait.is_null() {
            unsafe { UnregisterWaitEx(wait, INVALID_HANDLE_VALUE) };
        }
    }
}

impl<T> Drop for RegisteredWait<T> {
    fn drop(&mut self) {
        self.unregister();
    }
}
