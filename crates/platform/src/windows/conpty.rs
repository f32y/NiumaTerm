use std::ffi::OsString;
use std::io::{Error, Result};
use std::os::windows::ffi::OsStrExt as _;
use std::os::windows::io::IntoRawHandle;
use std::{mem, ptr};

use tracing::*;
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_MORE_DATA, GetLastError, HANDLE, S_OK};
use windows_sys::Win32::System::Console::{COORD, HPCON};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_BASIC_PROCESS_ID_LIST, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectBasicProcessIdList, JobObjectExtendedLimitInformation, QueryInformationJobObject,
    SetInformationJobObject,
};
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows_sys::Win32::System::Threading::{
    CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW, EXTENDED_STARTUPINFO_PRESENT,
    InitializeProcThreadAttributeList, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, PROCESS_INFORMATION,
    ResumeThread, STARTF_USESTDHANDLES, STARTUPINFOEXW, STARTUPINFOW, UpdateProcThreadAttribute,
};
use windows_sys::core::{HRESULT, PWSTR};
use windows_sys::{s, w};

use crate::Winsize;
use crate::windows::child::ChildExitWatcher;
use crate::windows::pipes::{EventedAnonRead, EventedAnonWrite};
use crate::windows::{Pty, cmdline, win32_string};

/// Load the pseudoconsole API from conpty.dll if possible, otherwise use the
/// standard Windows API.
///
/// The conpty.dll from the Windows Terminal project
/// supports loading OpenConsole.exe, which offers many improvements and
/// bugfixes compared to the standard conpty that ships with Windows.
///
/// The conpty.dll and OpenConsole.exe files will be searched in PATH and in
/// the directory where Rio's executable is located.
type CreatePseudoConsoleFn =
    unsafe extern "system" fn(COORD, HANDLE, HANDLE, u32, *mut HPCON) -> HRESULT;
type ResizePseudoConsoleFn = unsafe extern "system" fn(HPCON, COORD) -> HRESULT;
type ClosePseudoConsoleFn = unsafe extern "system" fn(HPCON);

struct ConptyApi {
    create: CreatePseudoConsoleFn,
    resize: ResizePseudoConsoleFn,
    close: ClosePseudoConsoleFn,
}

impl ConptyApi {
    fn new() -> Self {
        // The bundled Windows Terminal ConPTY is mandatory: it implements the resize
        // quirk (no full-buffer repaint), so scrollback survives a window resize. The
        // in-box system ConPTY repaints the whole buffer and corrupts the history on
        // every resize, so it is no longer an accepted fallback. `build.rs` copies
        // `conpty.dll` + `OpenConsole.exe` next to the executable.
        let api = Self::load_conpty().expect(
            "bundled ConPTY failed to load: conpty.dll + OpenConsole.exe must sit next \
             to the executable (copied by pty's build.rs). The in-box system ConPTY \
             corrupts scrollback on resize and is not supported.",
        );
        {
            use std::io::Write as _;
            let path = std::env::temp_dir().join("rio_conpty.log");
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
            {
                let _ = writeln!(f, "[conpty] backend=conpty.dll");
            }
        }
        info!("Using bundled conpty.dll for pseudoconsole");
        api
    }

    /// Try loading ConptyApi from Windows Terminal's bundled ConPTY: newer WT
    /// ships it as `OpenConsoleProxy.dll` (which spawns `OpenConsole.exe`), older
    /// WT as `conpty.dll`. This newer ConPTY fully implements the resize quirk
    /// (no full-buffer repaint on resize), so scrollback survives a window
    /// resize — the in-box system ConPTY does not. Both are searched in PATH and
    /// rio's executable directory.
    fn load_conpty() -> Option<Self> {
        type LoadedFn = unsafe extern "system" fn() -> isize;
        unsafe {
            // Prefer our bundled conpty.dll (copied next to the exe by build.rs) so the
            // reflow behavior matches the calibrated engine patches; fall back to a
            // newer Windows Terminal OpenConsoleProxy.dll only if it is on the search
            // path.
            let mut hmodule = LoadLibraryW(w!("conpty.dll"));
            if hmodule.is_null() {
                hmodule = LoadLibraryW(w!("OpenConsoleProxy.dll"));
            }
            // Newer ConPTY (OpenConsoleProxy.dll) exports `Conpty`-prefixed names;
            // the older in-box conpty.dll uses the unprefixed names.
            let null = hmodule.is_null();
            let create_fn = if null {
                None
            } else {
                GetProcAddress(hmodule, s!("ConptyCreatePseudoConsole"))
                    .or_else(|| GetProcAddress(hmodule, s!("CreatePseudoConsole")))
            };
            let resize_fn = if null {
                None
            } else {
                GetProcAddress(hmodule, s!("ConptyResizePseudoConsole"))
                    .or_else(|| GetProcAddress(hmodule, s!("ResizePseudoConsole")))
            };
            let close_fn = if null {
                None
            } else {
                GetProcAddress(hmodule, s!("ConptyClosePseudoConsole"))
                    .or_else(|| GetProcAddress(hmodule, s!("ClosePseudoConsole")))
            };
            {
                use std::io::Write as _;
                let path = std::env::temp_dir().join("rio_conpty.log");
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                {
                    let _ = writeln!(
                        f,
                        "[load] hmod={} create={} resize={} close={}",
                        !hmodule.is_null(),
                        create_fn.is_some(),
                        resize_fn.is_some(),
                        close_fn.is_some()
                    );
                }
            }
            let create_fn = create_fn?;
            let resize_fn = resize_fn?;
            let close_fn = close_fn?;

            Some(Self {
                create: mem::transmute::<LoadedFn, CreatePseudoConsoleFn>(create_fn),
                resize: mem::transmute::<LoadedFn, ResizePseudoConsoleFn>(resize_fn),
                close: mem::transmute::<LoadedFn, ClosePseudoConsoleFn>(close_fn),
            })
        }
    }
}

/// RAII Pseudoconsole.
pub struct Conpty {
    pub handle: HPCON,
    api: ConptyApi,
    /// Job object holding the shell's process tree (`KILL_ON_JOB_CLOSE`),
    /// present when job management is enabled. Closing the handle on drop
    /// kills every process still in the job.
    job: Option<HANDLE>,
}

impl Drop for Conpty {
    fn drop(&mut self) {
        // XXX: This will block until the conout pipe is drained. Will cause a deadlock if the
        // conout pipe has already been dropped by this point.
        //
        // See PR #3084 and https://docs.microsoft.com/en-us/windows/console/closepseudoconsole.
        unsafe { (self.api.close)(self.handle) }
        // After the console teardown, closing the job reaps whatever is left
        // of the tree (detached/GUI descendants included).
        if let Some(job) = self.job.take() {
            unsafe { CloseHandle(job) };
        }
    }
}

// The ConPTY handle can be sent between threads.
unsafe impl Send for Conpty {}

pub fn new(
    shell: &str,
    working_directory: &Option<String>,
    columns: u16,
    rows: u16,
    environment_overrides: &[(String, String)],
    starting_title: Option<&str>,
) -> Result<Pty> {
    let api = ConptyApi::new();
    let use_job = crate::job_management();
    let mut pty_handle: HPCON = 0;

    // Passing 0 as the size parameter allows the "system default" buffer
    // size to be used. There may be small performance and memory advantages
    // to be gained by tuning this in the future, but it's likely a reasonable
    // start point.
    let (conout, conout_pty_handle) = miow::pipe::anonymous(0)?;
    let (conin_pty_handle, conin) = miow::pipe::anonymous(0)?;

    let winsize = Winsize {
        ws_row: rows as libc::c_ushort,
        ws_col: columns as libc::c_ushort,
        ws_xpixel: 0 as libc::c_ushort,
        ws_ypixel: 0 as libc::c_ushort,
    };

    // Create the Pseudo Console, using the pipes. Prefer Windows Terminal's
    // OpenConsoleProxy.dll (newer ConPTY) over the in-box one (loaded in
    // `ConptyApi::new`): its console rewrite no longer repaints the whole buffer
    // on resize the way the in-box ConPTY does, so the engine's scrollback
    // survives a window resize (in-box ConPTY clears it — Microsoft bug #3490).
    let coord: COORD = winsize.into();
    let result = unsafe {
        (api.create)(
            coord,
            conin_pty_handle.into_raw_handle() as HANDLE,
            conout_pty_handle.into_raw_handle() as HANDLE,
            0,
            &mut pty_handle as *mut _,
        )
    };

    assert_eq!(result, S_OK);

    let mut success;

    // Prepare child process startup info.

    let mut size: usize = 0;

    let mut startup_info_ex: STARTUPINFOEXW = unsafe { mem::zeroed() };

    // ConPTY projects this console title as OSC 0/2. Seeding it avoids exposing
    // the executable path before an application deliberately changes its title.
    let mut title = starting_title.map(win32_string);
    startup_info_ex.StartupInfo.lpTitle = title
        .as_mut()
        .map_or(std::ptr::null_mut(), |title| title.as_mut_ptr());

    startup_info_ex.StartupInfo.cb = mem::size_of::<STARTUPINFOEXW>() as u32;

    // Setting this flag but leaving all the handles as default (null) ensures the
    // PTY process does not inherit any handles from this Rio process.
    startup_info_ex.StartupInfo.dwFlags |= STARTF_USESTDHANDLES;

    // Create the appropriately sized thread attribute list.
    unsafe {
        let failure =
            InitializeProcThreadAttributeList(ptr::null_mut(), 1, 0, &mut size as *mut usize) > 0;

        // This call was expected to return false.
        if failure {
            return Err(Error::last_os_error());
        }
    }

    let mut attr_list: Box<[u8]> = vec![0; size].into_boxed_slice();

    // Set startup info's attribute list & initialize it
    //
    // Lint failure is spurious; it's because winapi's definition of PROC_THREAD_ATTRIBUTE_LIST
    // implies it is one pointer in size (32 or 64 bits) but really this is just a dummy value.
    // Casting a *mut u8 (pointer to 8 bit type) might therefore not be aligned correctly in
    // the compiler's eyes.
    #[allow(clippy::cast_ptr_alignment)]
    {
        startup_info_ex.lpAttributeList = attr_list.as_mut_ptr() as _;
    }

    unsafe {
        success = InitializeProcThreadAttributeList(
            startup_info_ex.lpAttributeList,
            1,
            0,
            &mut size as *mut usize,
        ) > 0;

        if !success {
            return Err(Error::last_os_error());
        }
    }

    // Set thread attribute list's Pseudo Console to the specified ConPTY.
    unsafe {
        success = UpdateProcThreadAttribute(
            startup_info_ex.lpAttributeList,
            0,
            PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
            pty_handle as *mut std::ffi::c_void,
            mem::size_of::<HPCON>(),
            ptr::null_mut(),
            ptr::null_mut(),
        ) > 0;

        if !success {
            return Err(Error::last_os_error());
        }
    }

    let cmdline = win32_string(&cmdline(shell));
    let cwd = working_directory.as_ref().map(win32_string);
    let mut environment = build_environment_block(environment_overrides);

    let mut proc_info: PROCESS_INFORMATION = unsafe { mem::zeroed() };
    unsafe {
        success = CreateProcessW(
            ptr::null(),
            cmdline.as_ptr() as PWSTR,
            ptr::null_mut(),
            ptr::null_mut(),
            false as i32,
            // Suspended start when job management is on: the shell must be in
            // the job before it can spawn children, or an early child escapes.
            if use_job {
                EXTENDED_STARTUPINFO_PRESENT | CREATE_SUSPENDED
            } else {
                EXTENDED_STARTUPINFO_PRESENT
            } | CREATE_UNICODE_ENVIRONMENT,
            environment.as_mut_ptr().cast(),
            cwd.as_ref().map_or_else(ptr::null, |s| s.as_ptr()),
            &mut startup_info_ex.StartupInfo as *mut STARTUPINFOW,
            &mut proc_info as *mut PROCESS_INFORMATION,
        ) > 0;

        if !success {
            return Err(Error::last_os_error());
        }
    }

    let job = if use_job {
        let job = unsafe { create_kill_on_close_job(proc_info.hProcess) };
        // The shell was created suspended; resume it whether or not the job
        // setup succeeded (failure degrades to unmanaged, it must not hang).
        unsafe {
            ResumeThread(proc_info.hThread);
            CloseHandle(proc_info.hThread);
        }
        job
    } else {
        None
    };

    let conin = EventedAnonWrite::new(conin);
    let conout = EventedAnonRead::new(conout);

    let child_watcher = ChildExitWatcher::new(proc_info.hProcess)?;
    let conpty = Conpty {
        handle: pty_handle as HPCON,
        api,
        job,
    };

    Ok(Pty::new(conpty, conout, conin, child_watcher))
}

/// Build the sorted, double-NUL-terminated block required by `CreateProcessW`.
/// Windows environment names are case-insensitive, so an override replaces an
/// inherited spelling such as `Path` even when it is supplied as `PATH`.
fn build_environment_block(overrides: &[(String, String)]) -> Vec<u16> {
    let mut values: Vec<(OsString, OsString)> = std::env::vars_os().collect();
    // ConPTY supports 24-bit SGR colors, but Windows does not provide a standard
    // capability variable, so child TUIs otherwise downgrade computed RGB styles.
    values.retain(|(key, _)| !key.eq_ignore_ascii_case("COLORTERM"));
    values.push(("COLORTERM".into(), "truecolor".into()));
    for (key, value) in overrides {
        let folded = key.to_lowercase();
        values.retain(|(existing, _)| existing.to_string_lossy().to_lowercase() != folded);
        values.push((key.into(), value.into()));
    }
    values.sort_by(|(left, _), (right, _)| {
        left.to_string_lossy()
            .to_lowercase()
            .cmp(&right.to_string_lossy().to_lowercase())
    });

    let mut block = Vec::new();
    for (key, value) in values {
        block.extend(key.encode_wide());
        block.push('=' as u16);
        block.extend(value.encode_wide());
        block.push(0);
    }
    block.push(0);
    block
}

/// True when the job contains more than one process — i.e. the shell has
/// live descendants and closing the job would kill more than the shell
/// itself. `job` is a raw Job Object handle (`Pty::job_handle`).
pub fn job_has_other_processes(job: isize) -> bool {
    job_other_process_count(job) > 0
}

/// Number of processes in the job beyond the shell itself. `job` is a raw
/// Job Object handle (`Pty::job_handle`). 0 on query failure.
pub fn job_other_process_count(job: isize) -> usize {
    // Room for a few pids; on ERROR_MORE_DATA the header's
    // NumberOfAssignedProcesses still holds the full count.
    #[repr(C)]
    struct PidListBuf {
        list: JOBOBJECT_BASIC_PROCESS_ID_LIST,
        _extra: [usize; 7],
    }
    let mut buf: PidListBuf = unsafe { mem::zeroed() };
    let ok = unsafe {
        QueryInformationJobObject(
            job as HANDLE,
            JobObjectBasicProcessIdList,
            &mut buf as *mut _ as *mut std::ffi::c_void,
            mem::size_of::<PidListBuf>() as u32,
            ptr::null_mut(),
        )
    };
    if ok != 0 || unsafe { GetLastError() } == ERROR_MORE_DATA {
        (buf.list.NumberOfAssignedProcesses as usize).saturating_sub(1)
    } else {
        0
    }
}

/// Create a Job Object with `KILL_ON_JOB_CLOSE` and put `process` in it, so
/// the process tree dies with the job handle. Returns `None` (with a warning)
/// on failure — the shell then runs unmanaged, like with the setting off.
unsafe fn create_kill_on_close_job(process: HANDLE) -> Option<HANDLE> {
    unsafe {
        let job = CreateJobObjectW(ptr::null(), ptr::null());
        if job.is_null() {
            warn!("CreateJobObjectW failed: {}", Error::last_os_error());
            return None;
        }

        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let ok = SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const std::ffi::c_void,
            mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        ) > 0;
        if !ok {
            warn!("SetInformationJobObject failed: {}", Error::last_os_error());
            CloseHandle(job);
            return None;
        }

        if AssignProcessToJobObject(job, process) == 0 {
            warn!(
                "AssignProcessToJobObject failed: {}",
                Error::last_os_error()
            );
            CloseHandle(job);
            return None;
        }

        Some(job)
    }
}

impl Conpty {
    pub(crate) fn job(&self) -> Option<HANDLE> {
        self.job
    }

    pub fn on_resize(&mut self, window_size: Winsize) {
        let result = unsafe { (self.api.resize)(self.handle, window_size.into()) };
        assert_eq!(result, S_OK);
    }
}

impl From<Winsize> for COORD {
    fn from(window_size: Winsize) -> Self {
        let lines = window_size.ws_row;
        let columns = window_size.ws_col;
        COORD {
            X: columns as i16,
            Y: lines as i16,
        }
    }
}

#[cfg(test)]
mod environment_tests {
    use std::{mem, ptr};

    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        CREATE_UNICODE_ENVIRONMENT, CreateProcessW, INFINITE, PROCESS_INFORMATION, STARTUPINFOW,
        WaitForSingleObject,
    };

    use super::build_environment_block;

    fn entries(block: &[u16]) -> Vec<String> {
        block[..block.len() - 1]
            .split(|unit| *unit == 0)
            .filter(|entry| !entry.is_empty())
            .map(String::from_utf16_lossy)
            .collect()
    }

    #[test]
    fn overrides_replace_names_case_insensitively_without_mutating_parent() {
        let key = "NMT_PTY_ENV_REPLACEMENT_TEST";
        unsafe { std::env::set_var(key, "parent") };
        let block = build_environment_block(&[(key.to_lowercase(), "child".into())]);
        let matching: Vec<_> = entries(&block)
            .into_iter()
            .filter(|entry| entry.to_lowercase().starts_with(&key.to_lowercase()))
            .collect();
        assert_eq!(matching, [format!("{}=child", key.to_lowercase())]);
        assert_eq!(std::env::var(key).as_deref(), Ok("parent"));
        unsafe { std::env::remove_var(key) };
    }

    #[test]
    fn block_preserves_unrelated_values_and_unicode() {
        let inherited = "NMT_PTY_ENV_PRESERVE_TEST";
        unsafe { std::env::set_var(inherited, "kept") };
        let block = build_environment_block(&[("NMT_UNICODE".into(), "牛马终端🦀".into())]);
        let entries = entries(&block);
        assert!(
            entries
                .iter()
                .any(|entry| entry == &format!("{inherited}=kept"))
        );
        assert!(
            entries
                .iter()
                .any(|entry| entry == "NMT_UNICODE=牛马终端🦀")
        );
        unsafe { std::env::remove_var(inherited) };
    }

    #[test]
    fn block_advertises_truecolor_by_default() {
        let block = build_environment_block(&[]);
        assert!(
            entries(&block)
                .iter()
                .any(|entry| entry.eq_ignore_ascii_case("COLORTERM=truecolor"))
        );
    }

    #[test]
    fn block_is_sorted_case_insensitively_and_double_nul_terminated() {
        let block = build_environment_block(&[
            ("zz_nmt".into(), "z".into()),
            ("AA_NMT".into(), "a".into()),
        ]);
        assert!(block.len() >= 2);
        assert_eq!(&block[block.len() - 2..], &[0, 0]);
        let entries = entries(&block);
        let folded: Vec<_> = entries
            .iter()
            .map(|entry| {
                entry
                    .split_once('=')
                    .map_or(entry.as_str(), |(key, _)| key)
                    .to_lowercase()
            })
            .collect();
        assert!(folded.windows(2).all(|pair| pair[0] <= pair[1]));
    }

    #[test]
    fn create_process_receives_exact_agent_overrides() {
        let output = std::env::temp_dir().join(format!("nmt-pty-env-{}.txt", std::process::id()));
        let _ = std::fs::remove_file(&output);
        let overrides = [
            ("NMT_AGENT_ROUTE".into(), "route-exact".into()),
            ("NMT_AGENT_HOOK_TOKEN".into(), "token-exact".into()),
            ("NMT_AGENT_HOOK_VERSION".into(), "1".into()),
        ];
        let mut environment = build_environment_block(&overrides);
        let command = format!(
            "cmd.exe /d /c (echo %NMT_AGENT_ROUTE%&echo %NMT_AGENT_HOOK_TOKEN%&echo %NMT_AGENT_HOOK_VERSION%)>\"{}\"",
            output.display()
        );
        let mut command: Vec<u16> = command.encode_utf16().chain([0]).collect();
        let mut startup: STARTUPINFOW = unsafe { mem::zeroed() };
        startup.cb = mem::size_of::<STARTUPINFOW>() as u32;
        let mut process: PROCESS_INFORMATION = unsafe { mem::zeroed() };
        let created = unsafe {
            CreateProcessW(
                ptr::null(),
                command.as_mut_ptr(),
                ptr::null_mut(),
                ptr::null_mut(),
                0,
                CREATE_UNICODE_ENVIRONMENT,
                environment.as_mut_ptr().cast(),
                ptr::null(),
                &startup,
                &mut process,
            )
        };
        assert_ne!(created, 0, "{}", std::io::Error::last_os_error());
        unsafe {
            WaitForSingleObject(process.hProcess, INFINITE);
            CloseHandle(process.hThread);
            CloseHandle(process.hProcess);
        }
        let values = std::fs::read_to_string(&output).unwrap();
        let _ = std::fs::remove_file(output);
        assert_eq!(
            values.lines().collect::<Vec<_>>(),
            ["route-exact", "token-exact", "1"]
        );
    }
}
