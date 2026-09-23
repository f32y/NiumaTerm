pub(crate) mod login_shell;

use std::io;
use std::mem::size_of_val;
use std::os::raw::c_int;

use libc::__error;

/// Bindings for libproc.
mod sys {
    use std::os::raw::{c_int, c_void};

    unsafe extern "C" {
        pub fn proc_listpgrppids(pgrpid: c_int, buffer: *mut c_void, buffersize: c_int) -> c_int;
    }
}

/// Count group members from a populated buffer: a null-buffer query estimates
/// all system processes before the kernel applies its group filter.
/// See Apple's implementation:
/// https://github.com/apple-oss-distributions/xnu/blob/main/bsd/kern/proc_info.c
pub fn process_group_count(pgid: c_int) -> io::Result<usize> {
    let mut pids: Vec<c_int> = vec![0; 32];

    loop {
        let bytes = c_int::try_from(size_of_val(pids.as_slice()))
            .map_err(|_| io::Error::other("process list exceeds the native buffer limit"))?;

        // libproc returns a count, and reports errors as zero plus errno.
        // Clear errno so an empty group cannot inherit an earlier call's error.
        // https://github.com/apple-oss-distributions/xnu/blob/main/libsyscall/wrappers/libproc/libproc.c
        // SAFETY: errno is thread-local and the buffer has the supplied byte size.
        let count = unsafe {
            *__error() = 0;

            sys::proc_listpgrppids(pgid, pids.as_mut_ptr().cast(), bytes)
        };

        let error = io::Error::last_os_error();

        if count < 0 || (count == 0 && error.raw_os_error() != Some(0)) {
            return Err(error);
        }

        let count = count as usize;

        if count < pids.len() {
            return Ok(count);
        }

        let next = pids
            .len()
            .checked_mul(2)
            .filter(|len| *len <= c_int::MAX as usize / size_of::<c_int>())
            .ok_or_else(|| io::Error::other("process list exceeds the native buffer limit"))?;

        pids.resize(next, 0);
    }
}
