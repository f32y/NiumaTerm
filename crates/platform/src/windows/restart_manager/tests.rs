use std::collections::VecDeque;
use std::ffi::OsStr;
use std::io::{self, BufRead as _, BufReader, Read as _, Write as _};
use std::os::windows::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::{env, fs, process as current_process, ptr};

use parking_lot::Mutex;
use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_MORE_DATA, ERROR_SUCCESS, FreeLibrary,
};
use windows_sys::Win32::System::LibraryLoader::LoadLibraryW;
use windows_sys::Win32::System::RestartManager::{
    RM_PROCESS_INFO, RM_WRITE_STATUS_CALLBACK, RmExplorer, RmRebootReasonPermissionDenied,
};

use crate::windows::restart_manager::{
    Api, ApplicationKind, Operation, RestartManagerError, RestartManagerSession, Session,
};
use crate::windows::self_update::discard_previous;

#[derive(Clone)]
struct ScriptedApi {
    state: Arc<Mutex<State>>,
}

struct ListReply {
    code: u32,
    processes: Vec<RM_PROCESS_INFO>,
    needed: u32,
    reboot_reasons: u32,
}

struct State {
    start_code: u32,
    register_code: u32,
    shutdown_code: u32,
    restart_code: u32,
    list: VecDeque<ListReply>,
    ended: usize,
    shutdown_flags: Vec<u32>,
    restart_flags: Vec<u32>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            start_code: ERROR_SUCCESS,
            register_code: ERROR_SUCCESS,
            shutdown_code: ERROR_SUCCESS,
            restart_code: ERROR_SUCCESS,
            list: VecDeque::new(),
            ended: 0,
            shutdown_flags: Vec::new(),
            restart_flags: Vec::new(),
        }
    }
}

impl ScriptedApi {
    fn new(state: State) -> (Self, Arc<Mutex<State>>) {
        let state = Arc::new(Mutex::new(state));
        (
            Self {
                state: state.clone(),
            },
            state,
        )
    }
}

impl Api for ScriptedApi {
    fn start_session(&self, handle: *mut u32, _key: *mut u16) -> u32 {
        let state = self.state.lock();
        unsafe { *handle = 42 };
        state.start_code
    }

    fn end_session(&self, _handle: u32) -> u32 {
        self.state.lock().ended += 1;
        ERROR_SUCCESS
    }

    fn register_files(&self, _handle: u32, paths: &[*const u16]) -> u32 {
        assert!(!paths.is_empty());
        assert!(paths.iter().all(|path| !path.is_null()));
        self.state.lock().register_code
    }

    fn get_list(
        &self,
        _handle: u32,
        needed: *mut u32,
        count: *mut u32,
        processes: *mut RM_PROCESS_INFO,
        reboot_reasons: *mut u32,
    ) -> u32 {
        let reply = self.state.lock().list.pop_front().unwrap();
        unsafe {
            *needed = reply.needed;
            *reboot_reasons = reply.reboot_reasons;
            if !processes.is_null() {
                let capacity = *count as usize;
                let copied = capacity.min(reply.processes.len());
                ptr::copy_nonoverlapping(reply.processes.as_ptr(), processes, copied);
                *count = copied as u32;
            }
        }
        reply.code
    }

    fn shutdown(&self, _handle: u32, flags: u32, _callback: RM_WRITE_STATUS_CALLBACK) -> u32 {
        let mut state = self.state.lock();
        state.shutdown_flags.push(flags);
        state.shutdown_code
    }

    fn restart(&self, _handle: u32, flags: u32, _callback: RM_WRITE_STATUS_CALLBACK) -> u32 {
        let mut state = self.state.lock();
        state.restart_flags.push(flags);
        state.restart_code
    }
}

fn process(name: &str, pid: u32) -> RM_PROCESS_INFO {
    let mut process = RM_PROCESS_INFO {
        ApplicationType: RmExplorer,
        bRestartable: 1,
        TSSessionId: 7,
        ..Default::default()
    };
    process.Process.dwProcessId = pid;
    for (target, source) in process.strAppName.iter_mut().zip(name.encode_utf16()) {
        *target = source;
    }
    process
}

fn integration_scratch() -> PathBuf {
    let directory = env::temp_dir().join(format!(
        "nmt-restart-manager-integration-{}",
        current_process::id()
    ));
    let _ = fs::remove_dir_all(&directory);
    fs::create_dir_all(&directory).unwrap();
    directory
}

#[test]
fn growing_process_list_is_retried_and_decoded() {
    let explorer = process("Windows Explorer", 123);
    let state = State {
        list: VecDeque::from([
            ListReply {
                code: ERROR_MORE_DATA,
                processes: Vec::new(),
                needed: 1,
                reboot_reasons: 0,
            },
            ListReply {
                code: ERROR_MORE_DATA,
                processes: Vec::new(),
                needed: 2,
                reboot_reasons: 0,
            },
            ListReply {
                code: ERROR_SUCCESS,
                processes: vec![explorer],
                needed: 1,
                reboot_reasons: RmRebootReasonPermissionDenied as u32,
            },
        ]),
        ..Default::default()
    };
    let (api, shared) = ScriptedApi::new(state);
    let session =
        Session::for_files(api, &[Path::new(r"C:\NiumaTerm\NmtShellExtension.dll")]).unwrap();

    let usage = session.file_usage().unwrap();

    assert_eq!(usage.applications.len(), 1);
    assert_eq!(usage.applications[0].name, "Windows Explorer");
    assert_eq!(usage.applications[0].process_id, 123);
    assert_eq!(usage.applications[0].kind, ApplicationKind::Explorer);
    assert_eq!(usage.applications[0].terminal_session_id, Some(7));
    assert!(usage.applications[0].restartable);
    assert!(usage.reboot_reasons.permission_denied);
    drop(session);
    assert_eq!(shared.lock().ended, 1);
}

#[test]
fn registration_failure_still_ends_the_started_session() {
    let state = State {
        register_code: ERROR_ACCESS_DENIED,
        ..Default::default()
    };
    let (api, shared) = ScriptedApi::new(state);

    let error = Session::for_files(api, &[Path::new(r"C:\NiumaTerm\NmtShellExtension.dll")])
        .err()
        .unwrap();

    assert!(matches!(
        error,
        RestartManagerError::Windows {
            operation: Operation::RegisterResources,
            code: ERROR_ACCESS_DENIED,
        }
    ));
    assert_eq!(shared.lock().ended, 1);
}

#[test]
fn shutdown_and_restart_use_normal_action_flags() {
    let state = State {
        list: VecDeque::from([ListReply {
            code: ERROR_SUCCESS,
            processes: Vec::new(),
            needed: 0,
            reboot_reasons: 0,
        }]),
        ..Default::default()
    };
    let (api, shared) = ScriptedApi::new(state);
    let session =
        Session::for_files(api, &[Path::new(r"C:\NiumaTerm\NmtShellExtension.dll")]).unwrap();

    session.shutdown().unwrap();
    session.restart().unwrap();
    assert!(session.file_usage().unwrap().applications.is_empty());
    drop(session);

    let state = shared.lock();
    assert_eq!(state.shutdown_flags, [0]);
    assert_eq!(state.restart_flags, [0]);
    assert_eq!(state.ended, 1);
}

#[test]
fn shutdown_error_preserves_its_operation_and_code() {
    let state = State {
        shutdown_code: ERROR_ACCESS_DENIED,
        ..Default::default()
    };
    let (api, _) = ScriptedApi::new(state);
    let session =
        Session::for_files(api, &[Path::new(r"C:\NiumaTerm\NmtShellExtension.dll")]).unwrap();

    assert!(matches!(
        session.shutdown(),
        Err(RestartManagerError::Windows {
            operation: Operation::ShutdownApplications,
            code: ERROR_ACCESS_DENIED,
        })
    ));
}

#[test]
fn dll_holder_process() {
    let Some(path) = env::var_os("NMT_RESTART_MANAGER_TEST_DLL") else {
        return;
    };
    let wide = OsStr::new(&path)
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let module = unsafe { LoadLibraryW(wide.as_ptr()) };
    assert!(!module.is_null(), "load the isolated test DLL");

    println!("NMT_DLL_READY");
    io::stdout().flush().unwrap();
    let mut input = Vec::new();
    io::stdin().read_to_end(&mut input).unwrap();

    assert_ne!(unsafe { FreeLibrary(module) }, 0);
}

#[test]
fn a_loaded_dll_is_reported_and_old_copy_cleans_up_after_exit() {
    let scratch = integration_scratch();
    let system_root = env::var_os("WINDIR").expect("Windows directory");
    let source = PathBuf::from(system_root).join(r"System32\version.dll");
    let target = scratch.join("NmtShellExtension.dll");
    let previous = scratch.join("NmtShellExtension.dll.nmt-previous");
    fs::copy(&source, &target).unwrap();

    let mut child = Command::new(env::current_exe().unwrap())
        .args([
            "--exact",
            "windows::restart_manager::tests::dll_holder_process",
            "--nocapture",
        ])
        .env("NMT_RESTART_MANAGER_TEST_DLL", &target)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let child_input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let mut ready = false;
    let mut line = String::new();
    while output.read_line(&mut line).unwrap() != 0 {
        if line.contains("NMT_DLL_READY") {
            ready = true;
            break;
        }
        line.clear();
    }
    assert!(ready, "child process loaded the isolated DLL");

    let usage = RestartManagerSession::for_files(&[&target])
        .unwrap()
        .file_usage()
        .unwrap();
    assert!(
        usage
            .applications
            .iter()
            .any(|application| application.process_id == child.id())
    );

    fs::rename(&target, &previous).unwrap();
    fs::copy(&source, &target).unwrap();
    discard_previous(&scratch);
    assert!(previous.exists());

    drop(child_input);
    assert!(child.wait().unwrap().success());
    discard_previous(&scratch);

    assert!(target.exists());
    assert!(!previous.exists());
    fs::remove_dir_all(scratch).unwrap();
}
