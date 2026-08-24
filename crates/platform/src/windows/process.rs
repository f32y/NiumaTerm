use std::ffi::OsStr;
use std::os::windows::io::AsRawHandle as _;
use std::os::windows::process::{CommandExt as _, ExitStatusExt as _};
use std::process::{Child, Command, ExitStatus};
use std::sync::{Arc, Weak};
use std::{ffi, io, mem, ptr};

use windows_sys::Win32::Foundation::{CloseHandle, ERROR_MORE_DATA, GetLastError, HANDLE};
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
        Self::attach(child).map_err(|error| {
            let _ = child.kill();
            let _ = child.wait();
            error
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
