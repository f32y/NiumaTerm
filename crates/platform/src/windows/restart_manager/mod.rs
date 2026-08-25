//! Finding and managing applications that use files an update will replace.

use std::error::Error;
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::{fmt, ptr};

use windows_sys::Win32::Foundation::{ERROR_MORE_DATA, ERROR_SUCCESS, WIN32_ERROR};
use windows_sys::Win32::System::RestartManager::{
    CCH_RM_SESSION_KEY, RM_PROCESS_INFO, RM_WRITE_STATUS_CALLBACK, RmConsole, RmCritical,
    RmEndSession, RmExplorer, RmGetList, RmMainWindow, RmOtherWindow,
    RmRebootReasonCriticalProcess, RmRebootReasonCriticalService, RmRebootReasonDetectedSelf,
    RmRebootReasonPermissionDenied, RmRebootReasonSessionMismatch, RmRegisterResources, RmRestart,
    RmService, RmShutdown, RmStartSession,
};

#[cfg(test)]
mod tests;

const LIST_RETRIES: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplicationKind {
    Unknown,
    MainWindow,
    OtherWindow,
    Service,
    Explorer,
    Console,
    Critical,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ApplicationStatus(u32);

impl ApplicationStatus {
    pub fn from_bits(bits: u32) -> Self {
        Self(bits)
    }

    pub fn bits(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AffectedApplication {
    pub name: String,
    pub service_name: Option<String>,
    pub process_id: u32,
    pub kind: ApplicationKind,
    pub status: ApplicationStatus,
    pub terminal_session_id: Option<u32>,
    pub restartable: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RebootReasons {
    pub permission_denied: bool,
    pub session_mismatch: bool,
    pub critical_process: bool,
    pub critical_service: bool,
    pub detected_self: bool,
    pub unknown_bits: u32,
}

impl RebootReasons {
    pub fn is_empty(self) -> bool {
        self == Self::default()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileUsage {
    pub applications: Vec<AffectedApplication>,
    pub reboot_reasons: RebootReasons,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    StartSession,
    RegisterResources,
    ListApplications,
    ShutdownApplications,
    RestartApplications,
}

#[derive(Debug)]
pub enum RestartManagerError {
    NoFiles,
    RelativePath(PathBuf),
    Windows { operation: Operation, code: u32 },
}

impl RestartManagerError {
    pub fn windows_code(&self) -> Option<u32> {
        match self {
            Self::Windows { code, .. } => Some(*code),
            Self::NoFiles | Self::RelativePath(_) => None,
        }
    }
}

impl fmt::Display for RestartManagerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoFiles => write!(
                formatter,
                "a Restart Manager session needs at least one file"
            ),
            Self::RelativePath(path) => {
                write!(
                    formatter,
                    "Restart Manager needs an absolute path: {}",
                    path.display()
                )
            }
            Self::Windows { operation, code } => {
                write!(
                    formatter,
                    "Restart Manager {operation:?} failed with Windows error {code}"
                )
            }
        }
    }
}

impl Error for RestartManagerError {}

pub struct RestartManagerSession(Session<SystemApi>);

impl RestartManagerSession {
    pub fn for_files(paths: &[&Path]) -> Result<Self, RestartManagerError> {
        Session::for_files(SystemApi, paths).map(Self)
    }

    pub fn file_usage(&self) -> Result<FileUsage, RestartManagerError> {
        self.0.file_usage()
    }

    pub fn shutdown(&self) -> Result<(), RestartManagerError> {
        self.0.shutdown()
    }

    pub fn restart(&self) -> Result<(), RestartManagerError> {
        self.0.restart()
    }
}

struct Session<A: Api> {
    api: A,
    handle: u32,
}

impl<A: Api> Session<A> {
    fn for_files(api: A, paths: &[&Path]) -> Result<Self, RestartManagerError> {
        if paths.is_empty() {
            return Err(RestartManagerError::NoFiles);
        }
        if let Some(path) = paths.iter().find(|path| !path.is_absolute()) {
            return Err(RestartManagerError::RelativePath((*path).to_path_buf()));
        }

        let mut handle = 0;
        let mut key = [0u16; CCH_RM_SESSION_KEY as usize + 1];
        let code = api.start_session(&mut handle, key.as_mut_ptr());
        check(Operation::StartSession, code)?;

        let session = Self { api, handle };
        session.register_files(paths)?;
        Ok(session)
    }

    fn register_files(&self, paths: &[&Path]) -> Result<(), RestartManagerError> {
        let wide_paths = paths
            .iter()
            .map(|path| wide(path.as_os_str()))
            .collect::<Vec<_>>();
        let pointers = wide_paths
            .iter()
            .map(|path| path.as_ptr())
            .collect::<Vec<_>>();
        let code = self.api.register_files(self.handle, &pointers);
        check(Operation::RegisterResources, code)
    }

    fn file_usage(&self) -> Result<FileUsage, RestartManagerError> {
        let mut needed = 0;
        let mut count = 0;
        let mut reboot_reasons = 0;
        let code = self.api.get_list(
            self.handle,
            &mut needed,
            &mut count,
            ptr::null_mut(),
            &mut reboot_reasons,
        );

        if code == ERROR_SUCCESS {
            return Ok(FileUsage {
                applications: Vec::new(),
                reboot_reasons: decode_reboot_reasons(reboot_reasons),
            });
        }
        if code != ERROR_MORE_DATA {
            return Err(windows_error(Operation::ListApplications, code));
        }

        for _ in 0..LIST_RETRIES {
            let mut processes = vec![RM_PROCESS_INFO::default(); needed as usize];
            count = processes.len() as u32;
            let code = self.api.get_list(
                self.handle,
                &mut needed,
                &mut count,
                processes.as_mut_ptr(),
                &mut reboot_reasons,
            );
            if code == ERROR_MORE_DATA {
                continue;
            }
            check(Operation::ListApplications, code)?;
            processes.truncate(count as usize);
            return Ok(FileUsage {
                applications: processes.into_iter().map(decode_application).collect(),
                reboot_reasons: decode_reboot_reasons(reboot_reasons),
            });
        }

        Err(windows_error(Operation::ListApplications, ERROR_MORE_DATA))
    }

    fn shutdown(&self) -> Result<(), RestartManagerError> {
        check(
            Operation::ShutdownApplications,
            self.api.shutdown(self.handle, 0, None),
        )
    }

    fn restart(&self) -> Result<(), RestartManagerError> {
        check(
            Operation::RestartApplications,
            self.api.restart(self.handle, 0, None),
        )
    }
}

impl<A: Api> Drop for Session<A> {
    fn drop(&mut self) {
        let _ = self.api.end_session(self.handle);
    }
}

fn check(operation: Operation, code: WIN32_ERROR) -> Result<(), RestartManagerError> {
    if code == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(windows_error(operation, code))
    }
}

fn windows_error(operation: Operation, code: WIN32_ERROR) -> RestartManagerError {
    RestartManagerError::Windows { operation, code }
}

fn wide(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(Some(0)).collect()
}

fn wide_text(value: &[u16]) -> String {
    let end = value
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(value.len());
    String::from_utf16_lossy(&value[..end])
}

fn decode_application(process: RM_PROCESS_INFO) -> AffectedApplication {
    let service_name = wide_text(&process.strServiceShortName);
    AffectedApplication {
        name: wide_text(&process.strAppName),
        service_name: (!service_name.is_empty()).then_some(service_name),
        process_id: process.Process.dwProcessId,
        kind: application_kind(process.ApplicationType),
        status: ApplicationStatus(process.AppStatus),
        terminal_session_id: (process.TSSessionId != u32::MAX).then_some(process.TSSessionId),
        restartable: process.bRestartable != 0,
    }
}

fn application_kind(kind: i32) -> ApplicationKind {
    if kind == RmMainWindow {
        ApplicationKind::MainWindow
    } else if kind == RmOtherWindow {
        ApplicationKind::OtherWindow
    } else if kind == RmService {
        ApplicationKind::Service
    } else if kind == RmExplorer {
        ApplicationKind::Explorer
    } else if kind == RmConsole {
        ApplicationKind::Console
    } else if kind == RmCritical {
        ApplicationKind::Critical
    } else {
        ApplicationKind::Unknown
    }
}

fn decode_reboot_reasons(bits: u32) -> RebootReasons {
    let known = RmRebootReasonPermissionDenied as u32
        | RmRebootReasonSessionMismatch as u32
        | RmRebootReasonCriticalProcess as u32
        | RmRebootReasonCriticalService as u32
        | RmRebootReasonDetectedSelf as u32;
    RebootReasons {
        permission_denied: bits & RmRebootReasonPermissionDenied as u32 != 0,
        session_mismatch: bits & RmRebootReasonSessionMismatch as u32 != 0,
        critical_process: bits & RmRebootReasonCriticalProcess as u32 != 0,
        critical_service: bits & RmRebootReasonCriticalService as u32 != 0,
        detected_self: bits & RmRebootReasonDetectedSelf as u32 != 0,
        unknown_bits: bits & !known,
    }
}

trait Api {
    fn start_session(&self, handle: *mut u32, key: *mut u16) -> WIN32_ERROR;
    fn end_session(&self, handle: u32) -> WIN32_ERROR;
    fn register_files(&self, handle: u32, paths: &[*const u16]) -> WIN32_ERROR;
    fn get_list(
        &self,
        handle: u32,
        needed: *mut u32,
        count: *mut u32,
        processes: *mut RM_PROCESS_INFO,
        reboot_reasons: *mut u32,
    ) -> WIN32_ERROR;
    fn shutdown(&self, handle: u32, flags: u32, callback: RM_WRITE_STATUS_CALLBACK) -> WIN32_ERROR;
    fn restart(&self, handle: u32, flags: u32, callback: RM_WRITE_STATUS_CALLBACK) -> WIN32_ERROR;
}

struct SystemApi;

impl Api for SystemApi {
    fn start_session(&self, handle: *mut u32, key: *mut u16) -> WIN32_ERROR {
        unsafe { RmStartSession(handle, 0, key) }
    }

    fn end_session(&self, handle: u32) -> WIN32_ERROR {
        unsafe { RmEndSession(handle) }
    }

    fn register_files(&self, handle: u32, paths: &[*const u16]) -> WIN32_ERROR {
        unsafe {
            RmRegisterResources(
                handle,
                paths.len() as u32,
                paths.as_ptr(),
                0,
                ptr::null(),
                0,
                ptr::null(),
            )
        }
    }

    fn get_list(
        &self,
        handle: u32,
        needed: *mut u32,
        count: *mut u32,
        processes: *mut RM_PROCESS_INFO,
        reboot_reasons: *mut u32,
    ) -> WIN32_ERROR {
        unsafe { RmGetList(handle, needed, count, processes, reboot_reasons) }
    }

    fn shutdown(&self, handle: u32, flags: u32, callback: RM_WRITE_STATUS_CALLBACK) -> WIN32_ERROR {
        unsafe { RmShutdown(handle, flags, callback) }
    }

    fn restart(&self, handle: u32, flags: u32, callback: RM_WRITE_STATUS_CALLBACK) -> WIN32_ERROR {
        unsafe { RmRestart(handle, flags, callback) }
    }
}
