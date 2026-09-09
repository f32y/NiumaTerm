use std::ffi::OsStr;
use std::os::windows::io::AsRawHandle as _;
use std::os::windows::process::{CommandExt as _, ExitStatusExt as _};
use std::process::{Child, Command, ExitStatus};
use std::sync::{Arc, Weak};
use std::{env, ffi, io, mem, ptr, str};

use windows_sys::Win32::Foundation::{CloseHandle, ERROR_MORE_DATA, GetLastError, HANDLE};
use windows_sys::Win32::Globalization::{CP_OEMCP, MB_ERR_INVALID_CHARS, MultiByteToWideChar};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_BASIC_PROCESS_ID_LIST, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectBasicProcessIdList, JobObjectExtendedLimitInformation, QueryInformationJobObject,
    SetInformationJobObject,
};
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

pub fn hidden_command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    command.creation_flags(CREATE_NO_WINDOW);
    command
}

pub fn hidden_cmd_command(executable: impl AsRef<OsStr>) -> Command {
    let mut command = hidden_command("cmd.exe");
    command.args([OsStr::new("/D"), OsStr::new("/C")]);
    command.arg(executable);
    command
}

/// Text of one child's stdout or stderr capture.
///
/// Programs write UTF-8 to a pipe, but `cmd.exe` itself writes its own
/// diagnostics ("'x' is not recognized as an internal or external command")
/// in the console OEM code page, which on a Chinese system is GBK. Decoding
/// those bytes as UTF-8 turns every localized message into replacement
/// characters before it reaches the log. GBK text almost never validates as
/// UTF-8, so a failed UTF-8 decode is a reliable signal to retry with the OEM
/// code page. Bytes that neither decoder accepts fall back to lossy UTF-8 so a
/// truncated capture still yields its readable prefix.
pub fn decode_child_output(bytes: &[u8]) -> String {
    if let Ok(text) = str::from_utf8(bytes) {
        return text.to_owned();
    }
    decode_oem(bytes).unwrap_or_else(|| String::from_utf8_lossy(bytes).into_owned())
}

fn decode_oem(bytes: &[u8]) -> Option<String> {
    let len = i32::try_from(bytes.len()).ok()?;
    // SAFETY: the input pointer and length describe `bytes`; a null output
    // buffer asks only for the required length.
    let needed = unsafe {
        MultiByteToWideChar(
            CP_OEMCP,
            MB_ERR_INVALID_CHARS,
            bytes.as_ptr(),
            len,
            ptr::null_mut(),
            0,
        )
    };
    if needed <= 0 {
        return None;
    }
    let mut wide = vec![0_u16; needed as usize];
    // SAFETY: `wide` holds exactly the number of code units the first call
    // reported for the same input.
    let written = unsafe {
        MultiByteToWideChar(
            CP_OEMCP,
            MB_ERR_INVALID_CHARS,
            bytes.as_ptr(),
            len,
            wide.as_mut_ptr(),
            needed,
        )
    };
    if written <= 0 {
        return None;
    }
    Some(String::from_utf16_lossy(&wide[..written as usize]))
}

/// The value `name` carries in a child started by [`hidden_cmd_command`].
///
/// A Windows GUI process is started with the user's full environment, so a
/// child sees the same values this process does.
pub fn launch_env_var(name: &str) -> Option<ffi::OsString> {
    env::var_os(name)
}

pub fn exit_status_from_code(code: u32) -> ExitStatus {
    ExitStatus::from_raw(code)
}

/// Owns a Job Object that ends every assigned process when the final handle
/// closes. This keeps command shims and their descendants under one lifetime.
pub struct KillOnCloseJob(Arc<JobHandle>);

struct JobHandle(HANDLE);

#[derive(Clone)]
pub struct ProcessTree(Weak<JobHandle>);

// Kernel handles can move between threads, and this owner closes its handle once.
unsafe impl Send for JobHandle {}

// Shared references cannot close or duplicate the private handle.
unsafe impl Sync for JobHandle {}

impl KillOnCloseJob {
    pub fn attach(child: &Child) -> io::Result<Self> {
        unsafe { Self::attach_handle(child.as_raw_handle() as HANDLE) }
    }

    pub fn attach_or_kill(child: &mut Child) -> io::Result<Self> {
        Self::attach(child).inspect_err(|_| {
            let _ = child.kill();
            let _ = child.wait();
        })
    }

    pub(crate) unsafe fn attach_handle(process: HANDLE) -> io::Result<Self> {
        unsafe {
            let job = CreateJobObjectW(ptr::null(), ptr::null());
            if job.is_null() {
                return Err(io::Error::last_os_error());
            }

            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &raw const info as *const ffi::c_void,
                mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            ) == 0
            {
                let error = io::Error::last_os_error();
                CloseHandle(job);
                return Err(error);
            }

            if AssignProcessToJobObject(job, process) == 0 {
                let error = io::Error::last_os_error();
                CloseHandle(job);
                return Err(error);
            }

            Ok(Self(Arc::new(JobHandle(job))))
        }
    }

    pub(crate) fn process_tree(&self) -> ProcessTree {
        ProcessTree(Arc::downgrade(&self.0))
    }
}

impl Drop for JobHandle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

impl ProcessTree {
    pub fn process_count(&self) -> usize {
        self.0.upgrade().map_or(0, |job| query_process_count(job.0))
    }

    pub fn other_process_count(&self) -> usize {
        self.process_count().saturating_sub(1)
    }
}

fn query_process_count(job: HANDLE) -> usize {
    #[repr(C)]
    struct PidListBuffer {
        list: JOBOBJECT_BASIC_PROCESS_ID_LIST,
        extra: [usize; 7],
    }

    let mut buffer: PidListBuffer = unsafe { mem::zeroed() };
    let result = unsafe {
        QueryInformationJobObject(
            job,
            JobObjectBasicProcessIdList,
            &mut buffer as *mut _ as *mut ffi::c_void,
            mem::size_of::<PidListBuffer>() as u32,
            ptr::null_mut(),
        )
    };

    if result != 0 || unsafe { GetLastError() } == ERROR_MORE_DATA {
        buffer.list.NumberOfAssignedProcesses as usize
    } else {
        0
    }
}
