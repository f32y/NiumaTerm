use std::ffi::{self, OsString};
use std::io::{Error, Result};
use std::os::windows::ffi::OsStrExt as _;
use std::os::windows::io::IntoRawHandle;
use std::sync::mpsc;
use std::time::Duration;
use std::{env, mem, ptr, thread};

use libc::c_ushort;
use miow::pipe::anonymous;
use tracing::*;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, S_OK};
use windows_sys::Win32::System::Console::{COORD, HPCON};
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
use crate::windows::process::{KillOnCloseJob, ProcessTree};
use crate::windows::{Pty, cmdline, win32_string};

/// Load the pseudoconsole API from conpty.dll if possible, otherwise use the
/// standard Windows API.
///
/// The conpty.dll from the Windows Terminal project
/// supports loading OpenConsole.exe, which offers many improvements and
/// bugfixes compared to the standard conpty that ships with Windows.
///
/// The conpty.dll and OpenConsole.exe files will be searched in PATH and in
/// the directory where the NiumaTerm executable is located.
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

        info!("Using bundled conpty.dll for pseudoconsole");

        api
    }

    /// Try loading ConptyApi from Windows Terminal's bundled ConPTY: newer WT
    /// ships it as `OpenConsoleProxy.dll` (which spawns `OpenConsole.exe`), older
    /// WT as `conpty.dll`. This newer ConPTY fully implements the resize quirk
    /// (no full-buffer repaint on resize), so scrollback survives a window
    /// resize — the in-box system ConPTY does not. Both are searched in PATH and
    /// the NiumaTerm executable's directory.
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
    job: Option<KillOnCloseJob>,
}

/// How long the pseudoconsole close is given before the shell tree is ended
/// out from under it. A console whose client leaves closes in milliseconds, so
/// this only bounds the wait described on [`Conpty::drop`].
const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);

impl Drop for Conpty {
    fn drop(&mut self) {
        // ClosePseudoConsole returns once the console host has finished, which
        // needs the client to detach and the conout pipe to drain. This thread
        // is the one that was draining conout, so a shell that does not leave
        // on the console's close event blocks the call with nothing left to
        // release it: the job whose closure ends that shell is only reached
        // after the call returns, so the wait outlives its own remedy.
        //
        // See https://docs.microsoft.com/en-us/windows/console/closepseudoconsole.
        //
        // The close therefore runs on a scratch thread and the job is closed
        // whether or not it came back. Ending the tree is what frees a close
        // still waiting on it, and going in this order still lets a shell that
        // does leave on its own finish first.
        let (closed_tx, closed) = mpsc::channel();
        let handle = self.handle;
        let close = self.api.close;

        thread::spawn(move || {
            unsafe { close(handle) };

            let _ = closed_tx.send(());
        });

        if closed.recv_timeout(CLOSE_TIMEOUT).is_err() {
            warn!("conpty: the pseudoconsole is still closing; ending the shell tree");
        }

        // After the console teardown, closing the job reaps whatever is left
        // of the tree (detached/GUI descendants included).
        drop(self.job.take());
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
    manage_process_tree: bool,
) -> Result<Pty> {
    let api = ConptyApi::new();
    let mut pty_handle: HPCON = 0;

    // Passing 0 as the size parameter allows the "system default" buffer
    // size to be used. There may be small performance and memory advantages
    // to be gained by tuning this in the future, but it's likely a reasonable
    // start point.
    let (conout, conout_pty_handle) = anonymous(0)?;
    let (conin_pty_handle, conin) = anonymous(0)?;

    let winsize = Winsize {
        ws_row: rows as c_ushort,
        ws_col: columns as c_ushort,
        ws_xpixel: 0 as c_ushort,
        ws_ypixel: 0 as c_ushort,
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

    if result != S_OK {
        return Err(Error::other(format!(
            "CreatePseudoConsole failed: HRESULT {result:#010x}"
        )));
    }

    let mut success;

    // Prepare child process startup info.

    let mut size: usize = 0;

    let mut startup_info_ex: STARTUPINFOEXW = unsafe { mem::zeroed() };

    // ConPTY projects this console title as OSC 0/2. Seeding it avoids exposing
    // the executable path before an application deliberately changes its title.
    let mut title = starting_title.map(win32_string);

    startup_info_ex.StartupInfo.lpTitle = title
        .as_mut()
        .map_or(ptr::null_mut(), |title| title.as_mut_ptr());

    startup_info_ex.StartupInfo.cb = mem::size_of::<STARTUPINFOEXW>() as u32;

    // Setting this flag but leaving all the handles as default (null) ensures the
    // PTY process does not inherit any handles from this NiumaTerm process.
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
            pty_handle as *mut ffi::c_void,
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
            if manage_process_tree {
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

    let job = if manage_process_tree {
        let job = unsafe { KillOnCloseJob::attach_handle(proc_info.hProcess) }
            .map_err(|error| warn!("failed to create process-tree job: {error}"))
            .ok();

        // The shell was created suspended; resume it whether or not the job
        // setup succeeded (failure degrades to unmanaged, it must not hang).
        unsafe {
            ResumeThread(proc_info.hThread);
        }

        job
    } else {
        None
    };

    // The primary-thread handle has no further use in either path; leaving it
    // open would leak one thread handle per spawned PTY.
    unsafe {
        CloseHandle(proc_info.hThread);
    }

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
    let mut values: Vec<(OsString, OsString)> = env::vars_os().collect();
    // ConPTY supports 24-bit SGR colors, but Windows does not provide a standard
    // capability variable, so child TUIs otherwise downgrade computed RGB styles.
    values.retain(|(key, _)| !key.eq_ignore_ascii_case("COLORTERM"));

    values.push(("COLORTERM".into(), "truecolor".into()));

    // TERM_FEATURES=P advertises OSC 9;4 so progress-aware tools can distinguish a
    // transient status line from ordinary output instead of relying on CR heuristics.
    let mut term_features = env::var("TERM_FEATURES").unwrap_or_default();

    if !term_features.contains('P') {
        term_features.push('P');
    }

    values.retain(|(key, _)| !key.eq_ignore_ascii_case("TERM_FEATURES"));

    values.push(("TERM_FEATURES".into(), term_features.into()));

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

impl Conpty {
    pub(crate) fn process_tree(&self) -> Option<ProcessTree> {
        self.job.as_ref().map(KillOnCloseJob::process_tree)
    }

    pub fn on_resize(&mut self, window_size: Winsize) {
        let result = unsafe { (self.api.resize)(self.handle, window_size.into()) };

        // A failed resize leaves the console at its previous size; the session
        // itself is still healthy, so this must not take down the process.
        if result != S_OK {
            warn!("ResizePseudoConsole failed: HRESULT {result:#010x}");
        }
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
mod tests;
