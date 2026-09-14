// From: https://github.com/alacritty/alacritty/blob/04ea367e3baa7e51933e9a595da793b4c8a4aa8f/alacritty/src/macos/proc.rs

pub(crate) mod login_shell;

#[cfg(test)]
mod tests;

use std::ffi::{CStr, CString, IntoStringError, OsStr};

use std::fmt::{self, Display, Formatter};

use std::mem::{MaybeUninit, size_of_val};

use std::os::raw::c_int;

use std::path::{Path, PathBuf};

use std::{error, io, ptr};

use libc::{__error, c_void};

/// Bindings for libproc.
#[allow(non_camel_case_types)]
mod sys {
    use std::os::raw::{c_char, c_int, c_longlong, c_void};

    pub const PROC_PIDVNODEPATHINFO: c_int = 9;

    type gid_t = c_int;

    type off_t = c_longlong;

    type uid_t = c_int;

    type fsid_t = fsid;

    #[repr(C)]
    #[derive(Debug, Copy, Clone)]
    pub struct fsid {
        pub val: [i32; 2usize],
    }

    #[repr(C)]
    #[derive(Debug, Copy, Clone)]
    pub struct vinfo_stat {
        pub vst_dev: u32,
        pub vst_mode: u16,
        pub vst_nlink: u16,
        pub vst_ino: u64,
        pub vst_uid: uid_t,
        pub vst_gid: gid_t,
        pub vst_atime: i64,
        pub vst_atimensec: i64,
        pub vst_mtime: i64,
        pub vst_mtimensec: i64,
        pub vst_ctime: i64,
        pub vst_ctimensec: i64,
        pub vst_birthtime: i64,
        pub vst_birthtimensec: i64,
        pub vst_size: off_t,
        pub vst_blocks: i64,
        pub vst_blksize: i32,
        pub vst_flags: u32,
        pub vst_gen: u32,
        pub vst_rdev: u32,
        pub vst_qspare: [i64; 2usize],
    }

    #[repr(C)]
    #[derive(Debug, Copy, Clone)]
    pub struct vnode_info {
        pub vi_stat: vinfo_stat,
        pub vi_type: c_int,
        pub vi_pad: c_int,
        pub vi_fsid: fsid_t,
    }

    #[repr(C)]
    #[derive(Copy, Clone)]
    pub struct vnode_info_path {
        pub vip_vi: vnode_info,
        pub vip_path: [c_char; 1024usize],
    }

    #[repr(C)]
    #[derive(Copy, Clone)]
    pub struct proc_vnodepathinfo {
        pub pvi_cdir: vnode_info_path,
        pub pvi_rdir: vnode_info_path,
    }

    unsafe extern "C" {
        pub fn proc_pidpath(pid: c_int, buffer: *mut c_void, buffersize: u32) -> c_int;

        pub fn proc_pidinfo(
            pid: c_int,
            flavor: c_int,
            arg: u64,
            buffer: *mut c_void,
            buffersize: c_int,
        ) -> c_int;

        pub fn proc_listpgrppids(pgrpid: c_int, buffer: *mut c_void, buffersize: c_int) -> c_int;
    }
}

/// Error during working directory retrieval.
#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    /// Error converting into utf8 string.
    IntoString(IntoStringError),
    /// Expected return size didn't match libproc's.
    InvalidSize,
}

impl error::Error for Error {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        match self {
            Error::InvalidSize => None,
            Error::Io(err) => err.source(),
            Error::IntoString(err) => err.source(),
        }
    }
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Error::InvalidSize => write!(f, "Invalid proc_pidinfo return size"),
            Error::Io(err) => {
                write!(f, "Error getting current working directory: {err}")
            }
            Error::IntoString(err) => {
                write!(f, "Error when parsing current working directory: {err}")
            }
        }
    }
}

impl From<io::Error> for Error {
    fn from(val: io::Error) -> Self {
        Error::Io(val)
    }
}

impl From<IntoStringError> for Error {
    fn from(val: IntoStringError) -> Self {
        Error::IntoString(val)
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

pub fn macos_process_name(pid: c_int) -> String {
    let mut name = String::new();

    if pid >= 0 {
        let proc_path = proc_path(pid);

        name = Path::new(&proc_path)
            .file_name()
            .unwrap_or(OsStr::new(""))
            .to_str()
            .unwrap_or("")
            .to_string();
    }

    name
}

fn proc_path(pid: i32) -> String {
    let mut pathbuf: Vec<u8> = Vec::with_capacity(4 * 1024); // 4 * MAXPATHLEN

    #[allow(unused)]
    let mut ret: i32 = 0;

    let mut out = String::new();

    unsafe {
        ret = sys::proc_pidpath(
            pid,
            pathbuf.as_mut_ptr() as *mut c_void,
            pathbuf.capacity() as u32,
        );
    };

    if ret > 0 {
        unsafe {
            pathbuf.set_len(ret as usize);
        }

        out = String::from_utf8(pathbuf)
            .unwrap_or("An error occurred while retrieving process path".to_string())
    }

    out
}

pub fn macos_cwd(pid: c_int) -> Result<PathBuf, Error> {
    let mut info = MaybeUninit::<sys::proc_vnodepathinfo>::uninit();

    let info_ptr = info.as_mut_ptr() as *mut c_void;
    let size = size_of::<sys::proc_vnodepathinfo>() as c_int;

    let c_str = unsafe {
        let pidinfo_size = sys::proc_pidinfo(pid, sys::PROC_PIDVNODEPATHINFO, 0, info_ptr, size);

        match pidinfo_size {
            c if c < 0 => return Err(io::Error::last_os_error().into()),
            s if s != size => return Err(Error::InvalidSize),
            _ => CStr::from_ptr(info.assume_init().pvi_cdir.vip_path.as_ptr()),
        }
    };

    let c_string: CString = c_str.into();

    Ok(c_string.into_string().map(Into::into)?)
}
