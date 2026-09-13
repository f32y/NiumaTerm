#![cfg(unix)]
pub use crate::unix::process_exit::wait_for_exit;
pub use crate::unix::shell_integration::{
    is_shell_integration_registered, register_shell_integration, set_system_notification_enabled,
    shell_integration_dll_mismatched, system_notification_enabled, unregister_shell_integration,
};

pub(crate) use crate::unix::hook_command::{build_hook_command, hook_command_contains};
#[cfg(target_os = "macos")]
pub(crate) use crate::unix::notifier::request_authorization;
pub(crate) use crate::unix::notifier::{remove, show};
pub(crate) use crate::unix::shell::{default_shell, prompt_integration};

pub mod environment;
pub mod filesystem;
pub mod ipc;
pub mod process;
pub mod shell;
pub mod window;

pub(crate) mod library;

#[cfg(feature = "clipboard")]
mod clipboard;

mod process_exit;
mod shell_integration;

mod hook_command;

#[cfg(target_os = "macos")]
mod macos;
mod notifier;
mod signals;

use std::ffi::{CStr, CString, OsStr};
use std::fs::File;
use std::io::Error;
use std::mem::MaybeUninit;
use std::ops::Deref;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child as ChildProcess, Command, Stdio};
use std::sync::Arc;
use std::{env, error, io, ptr, str};

use dirs::home_dir;
use mio::unix::SourceFd;
use mio::{Interest, Poll, Token, Waker};
use signal_hook::consts as sigconsts;
use tracing::info;

use crate::unix::hook_command::single_quoted;
#[cfg(target_os = "macos")]
use crate::unix::macos::*;
use crate::unix::process::{KillOnCloseJob, ProcessTree};
use crate::unix::signals::Signals;
use crate::{
    APP_ID, ChildEvent, EventedPty, ProcessReadWrite, PtyOptions, Winsize, WinsizeBuilder,
};

#[cfg(all(target_os = "linux", not(target_env = "musl")))]
const TIOCSWINSZ: libc::c_ulong = 0x5414;

#[cfg(all(target_os = "linux", target_env = "musl"))]
const TIOCSWINSZ: libc::c_int = 0x5414;

#[cfg(target_os = "freebsd")]
const TIOCSWINSZ: libc::c_ulong = 0x80087467;

#[cfg(target_os = "macos")]
const TIOCSWINSZ: libc::c_ulong = 2148037735;

#[link(name = "util")]
unsafe extern "C" {
    fn forkpty(
        main: *mut libc::c_int,
        name: *mut libc::c_char,
        termp: *const libc::termios,
        winsize: *const Winsize,
    ) -> libc::pid_t;

    fn openpty(
        main: *mut libc::c_int,
        child: *mut libc::c_int,
        name: *mut libc::c_char,
        termp: *const libc::termios,
        winsize: *const Winsize,
    ) -> libc::pid_t;

    fn waitpid(pid: libc::pid_t, status: *mut libc::c_int, options: libc::c_int) -> libc::pid_t;

    fn ptsname(fd: *mut libc::c_int) -> *mut libc::c_char;
}

#[cfg(target_os = "macos")]
fn default_shell_command(shell: &str) {
    let command_shell_string = CString::new(shell).unwrap();
    let command_pointer = command_shell_string.as_ptr();
    let args = CString::new("--login").unwrap();
    let args_pointer = args.as_ptr();

    unsafe {
        libc::execvp(command_pointer, vec![args_pointer].as_ptr());
    }
}

#[cfg(not(target_os = "macos"))]
fn default_shell_command(shell: &str) {
    let command_shell_string = CString::new(shell).unwrap();
    let command_pointer = command_shell_string.as_ptr();

    unsafe {
        libc::execvp(command_pointer, vec![command_pointer, ptr::null()].as_ptr());
    }
}

pub struct Pty {
    pub child: Child,
    file: File,
    token: Token,
    signals_token: Token,
    signals: Signals,

    /// Present only for a managed PTY. Dropping it signals the shell's
    /// process group, which ends the descendants a bare `SIGHUP` to the shell
    /// would leave running.
    job: Option<KillOnCloseJob>,
}

impl Pty {
    /// A view of the shell's process group, or `None` when this PTY does not
    /// manage its child's descendants.
    pub fn process_tree(&self) -> Option<ProcessTree> {
        self.job.as_ref().map(KillOnCloseJob::process_tree)
    }
}

impl Deref for Pty {
    type Target = Child;

    fn deref(&self) -> &Child {
        &self.child
    }
}

impl io::Write for Pty {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match unsafe {
            libc::write(
                *self.child,
                buf.as_ptr() as *const _,
                buf.len() as libc::size_t,
            )
        } {
            n if n >= 0 => Ok(n as usize),
            _ => Err(io::Error::last_os_error()),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl io::Read for Pty {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match unsafe {
            libc::read(
                *self.child,
                buf.as_mut_ptr() as *mut _,
                buf.len() as libc::size_t,
            )
        } {
            n if n >= 0 => Ok(n as usize),
            _ => Err(io::Error::last_os_error()),
        }
    }
}

impl ProcessReadWrite for Pty {
    type Reader = File;

    type Writer = File;

    #[inline]
    fn reader(&mut self) -> &mut File {
        &mut self.file
    }

    #[inline]
    fn read_token(&self) -> Token {
        self.token
    }

    #[inline]
    fn writer(&mut self) -> &mut File {
        &mut self.file
    }

    #[inline]
    fn write_token(&self) -> Token {
        self.token
    }

    #[inline]
    fn set_winsize(&mut self, winsize: WinsizeBuilder) -> Result<(), io::Error> {
        self.child.set_winsize(winsize)
    }

    #[inline]
    fn register(
        &mut self,
        poll: &Poll,
        token: &mut dyn Iterator<Item = Token>,
        interest: Interest,
        _waker: &Arc<Waker>,
    ) -> io::Result<()> {
        // The pty fd is a real OS readiness source; no `Waker` needed on Unix.
        self.token = token.next().unwrap();

        poll.registry()
            .register(&mut SourceFd(&self.file.as_raw_fd()), self.token, interest)?;

        self.signals_token = token.next().unwrap();

        poll.registry()
            .register(&mut self.signals, self.signals_token, Interest::READABLE)
    }

    fn reregister(&mut self, poll: &Poll, interest: Interest) -> io::Result<()> {
        poll.registry()
            .reregister(&mut SourceFd(&self.file.as_raw_fd()), self.token, interest)?;

        poll.registry()
            .reregister(&mut self.signals, self.signals_token, Interest::READABLE)
    }

    fn deregister(&mut self, poll: &Poll) -> io::Result<()> {
        poll.registry()
            .deregister(&mut SourceFd(&self.file.as_raw_fd()))?;

        poll.registry().deregister(&mut self.signals)
    }

    #[inline]
    fn drain_ready(&self) -> Vec<Token> {
        // Unix has real OS readiness; the soft-ready set is Windows-only.
        Vec::new()
    }
}

/// The terminal type a session announces to its child.
///
/// It is stated rather than inherited, because neither way of starting the
/// application supplies a usable one. Started from another terminal, the
/// environment names that terminal rather than this one. Started from Finder or
/// the Dock, it names nothing at all: launchd exports no `TERM`, and
/// `/usr/bin/login` substitutes `network` for the missing value even under
/// `-p`. No terminfo entry by that name exists, so the shell concludes it
/// cannot address the cursor and reprints its prompt wherever the cursor
/// happens to sit instead of redrawing it in place. Every window resize then
/// leaves another copy of the prompt behind.
///
/// `xterm-256color` is the entry every system carries and describes what this
/// emulator does; `xterm` covers a terminfo database old enough to lack it.
fn terminal_type() -> &'static str {
    if terminfo_exists("xterm-256color") {
        "xterm-256color"
    } else {
        "xterm"
    }
}

// From alacritty: https://github.com/alacritty/alacritty/blob/2df8f860b960d7c96efaf4f059fe2fbbdce82bcc/alacritty_terminal/src/tty/mod.rs#L83
/// Check if a terminfo entry exists on the system.
pub fn terminfo_exists(terminfo: &str) -> bool {
    // Get first terminfo character for the parent directory.
    let first = terminfo.get(..1).unwrap_or_default();
    let first_hex = format!("{:x}", first.chars().next().unwrap_or_default() as usize);

    // Return true if the terminfo file exists at the specified location.
    macro_rules! check_path {
        ($path:expr) => {{
            let path: PathBuf = $path;
            if path.join(first).join(terminfo).exists()
                || path.join(&first_hex).join(terminfo).exists()
            {
                return true;
            }
        }};
    }

    if let Some(dir) = env::var_os("TERMINFO") {
        check_path!((&dir).into());
    } else if let Some(home) = home_dir() {
        check_path!(home.join(".terminfo"));
    }

    if let Ok(dirs) = env::var("TERMINFO_DIRS") {
        for dir in dirs.split(':') {
            check_path!(dir.into());
        }
    }

    if let Ok(prefix) = env::var("PREFIX") {
        let path: PathBuf = prefix.into();

        check_path!(path.join("etc/terminfo"));
        check_path!(path.join("lib/terminfo"));
        check_path!(path.join("share/terminfo"));
    }

    check_path!("/etc/terminfo".into());
    check_path!("/lib/terminfo".into());
    check_path!("/usr/share/terminfo".into());
    check_path!("/boot/system/data/terminfo".into());

    // No valid terminfo path has been found.
    false
}

pub fn create_termp(utf8: bool) -> libc::termios {
    // musl libc does not provide c_ispeed and c_ospeed fields in struct termios.
    #[cfg(target_os = "linux")]
    let mut term = libc::termios {
        c_iflag: libc::ICRNL | libc::IXON | libc::IXANY | libc::IMAXBEL | libc::BRKINT,
        c_oflag: libc::OPOST | libc::ONLCR,
        c_cflag: libc::CREAD | libc::CS8 | libc::HUPCL,
        c_lflag: libc::ICANON
            | libc::ISIG
            | libc::IEXTEN
            | libc::ECHO
            | libc::ECHOE
            | libc::ECHOK
            | libc::ECHOKE
            | libc::ECHOCTL,
        c_cc: Default::default(),
        #[cfg(not(target_env = "musl"))]
        c_ispeed: Default::default(),
        #[cfg(not(target_env = "musl"))]
        c_ospeed: Default::default(),
        #[cfg(target_env = "musl")]
        __c_ispeed: Default::default(),
        #[cfg(target_env = "musl")]
        __c_ospeed: Default::default(),
        c_line: 0,
    };

    #[cfg(any(target_os = "macos", target_os = "freebsd"))]
    let mut term = libc::termios {
        c_iflag: libc::ICRNL | libc::IXON | libc::IXANY | libc::IMAXBEL | libc::BRKINT,
        c_oflag: libc::OPOST | libc::ONLCR,
        c_cflag: libc::CREAD | libc::CS8 | libc::HUPCL,
        c_lflag: libc::ICANON
            | libc::ISIG
            | libc::IEXTEN
            | libc::ECHO
            | libc::ECHOE
            | libc::ECHOK
            | libc::ECHOKE
            | libc::ECHOCTL,
        c_cc: Default::default(),
        c_ispeed: Default::default(),
        c_ospeed: Default::default(),
    };

    #[cfg(not(target_os = "freebsd"))]
    {
        // Enable utf8 support if requested
        if utf8 {
            term.c_iflag |= libc::IUTF8;
        }
    }

    // Set supported terminal characters
    term.c_cc[libc::VEOF] = 4;
    term.c_cc[libc::VEOL] = 255;
    term.c_cc[libc::VEOL2] = 255;
    term.c_cc[libc::VERASE] = 0x7f;
    term.c_cc[libc::VWERASE] = 23;
    term.c_cc[libc::VKILL] = 21;
    term.c_cc[libc::VREPRINT] = 18;
    term.c_cc[libc::VINTR] = 3;
    term.c_cc[libc::VQUIT] = 0x1c;
    term.c_cc[libc::VSUSP] = 26;
    term.c_cc[libc::VSTART] = 17;
    term.c_cc[libc::VSTOP] = 19;
    term.c_cc[libc::VLNEXT] = 22;
    term.c_cc[libc::VDISCARD] = 15;
    term.c_cc[libc::VMIN] = 1;
    term.c_cc[libc::VTIME] = 0;

    #[cfg(target_os = "macos")]
    {
        term.c_cc[libc::VDSUSP] = 25;
        term.c_cc[libc::VSTATUS] = 20;
    }

    term
}

#[derive(Default)]
struct ShellUser {
    user: String,
    home: String,
    shell: String,
}

impl ShellUser {
    /// look for shell, username, longname, and home dir in the respective environment variables
    /// before falling back on looking in to `passwd`.
    fn from_env() -> Result<Self, Error> {
        let mut buf = [0; 1024];
        let pw = get_pw_entry(&mut buf);

        // The passwd entry only fills in what the environment does not carry,
        // so a session that exports all three never has to read it and a
        // failed read is reported only when something is actually missing.
        let (user, home, shell) = match (env::var("USER"), env::var("HOME"), env::var("SHELL")) {
            (Ok(user), Ok(home), Ok(shell)) => (user, home, shell),

            (user, home, shell) => {
                let pw = pw?;

                (
                    user.unwrap_or_else(|_| pw.name.to_owned()),
                    home.unwrap_or_else(|_| pw.dir.to_owned()),
                    shell.unwrap_or_else(|_| pw.shell.to_owned()),
                )
            }
        };

        Ok(Self { user, home, shell })
    }
}

/// Create a shell PTY with child-only environment overrides.
pub fn create_pty_with_env(options: PtyOptions<'_>) -> Result<Pty, Error> {
    create_pty_with_management(options, false)
}

/// Create a shell PTY whose child process tree is terminated when it is dropped.
pub fn create_managed_pty_with_env(options: PtyOptions<'_>) -> Result<Pty, Error> {
    let pty = create_pty_with_management(options, true)?;

    if pty.process_tree().is_none() {
        return Err(Error::other(
            "managed PTY could not contain its child process group",
        ));
    }

    Ok(pty)
}

/// Fail before spawning when the configured shell does not resolve to an
/// executable.
///
/// macOS launches the shell through `/usr/bin/login`, which always exists, so
/// an unusable shell would otherwise spawn successfully and die inside the
/// child where the caller only sees an empty terminal. Checking first keeps
/// the cause in the spawn result, which is where every platform reports it.
fn require_executable_shell(shell: &str) -> Result<(), Error> {
    which::which(shell).map(|_| ()).map_err(|error| {
        Error::new(
            io::ErrorKind::NotFound,
            format!("shell `{shell}` is not an executable program: {error}"),
        )
    })
}

/// The initial pixel size a PTY reports. The window has not been laid out
/// when the shell starts, and `set_winsize` carries the real dimensions from
/// the first resize onward; zero is the value programs already read as
/// "unknown".
const UNKNOWN_PIXEL_SIZE: u16 = 0;

/// Place `bootstrap` in the new terminal's input queue without it being seen.
///
/// The write happens before the child exists, so the bytes simply wait in the
/// line discipline's queue for the shell's first read. Echo is decided when a
/// character arrives, not when it is read, so clearing `ECHO` for the duration
/// of this write is enough to hide it — and the launch turns the shell's own
/// line editor off, because an editor would otherwise draw the line itself and
/// never consult `ECHO` at all.
fn queue_bootstrap(main: libc::c_int, child: libc::c_int, bootstrap: &str) -> Result<(), Error> {
    let bytes = bootstrap.as_bytes();
    let mut written = 0;

    while written < bytes.len() {
        // SAFETY: the slice outlives the call and the length is its remainder.
        let count = unsafe {
            libc::write(
                main,
                bytes[written..].as_ptr().cast(),
                bytes.len() - written,
            )
        };

        if count < 0 {
            return Err(Error::last_os_error());
        }

        written += count as usize;
    }

    // Hand the session the echo it expects, now that the one write that had to
    // stay invisible is already in the queue.
    let restored = create_termp(true);

    // SAFETY: `child` is the pty's terminal side, open for the whole call.
    if unsafe { libc::tcsetattr(child, libc::TCSANOW, &restored) } != 0 {
        return Err(Error::last_os_error());
    }

    Ok(())
}

fn create_pty_with_management(
    options: PtyOptions<'_>,
    manage_process_tree: bool,
) -> Result<Pty, Error> {
    let PtyOptions {
        shell,
        args,
        working_directory,
        columns,
        rows,
        environment_overrides,
        bootstrap,
        ..
    } = options;

    let (width, height) = (UNKNOWN_PIXEL_SIZE, UNKNOWN_PIXEL_SIZE);

    #[cfg(not(any(target_os = "macos", target_os = "freebsd")))]
    let mut take_controlling_terminal = true;

    #[cfg(any(target_os = "macos", target_os = "freebsd"))]
    let take_controlling_terminal = true;

    let mut main: libc::c_int = 0;
    let mut child: libc::c_int = 0;

    let winsize = Winsize {
        ws_row: rows as libc::c_ushort,
        ws_col: columns as libc::c_ushort,
        ws_xpixel: width as libc::c_ushort,
        ws_ypixel: height as libc::c_ushort,
    };

    let mut term = create_termp(true);

    if bootstrap.is_some() {
        term.c_lflag &= !libc::ECHO;
    }

    let res = unsafe {
        openpty(
            &mut main as *mut _,
            &mut child as *mut _,
            ptr::null_mut(),
            &term as *const libc::termios,
            &winsize as *const _,
        )
    };

    if res < 0 {
        return Err(Error::other("openpty failed"));
    }

    if let Some(bootstrap) = bootstrap {
        queue_bootstrap(main, child, bootstrap)?;
    }

    let mut shell_program = shell;

    let user = match ShellUser::from_env() {
        Ok(data) => data,

        Err(..) => ShellUser {
            shell: shell.to_string(),
            ..Default::default()
        },
    };

    if shell.is_empty() {
        shell_program = &user.shell;
    }

    require_executable_shell(shell_program)?;

    info!("spawn {:?} {:?}", shell_program, args);

    let mut builder = {
        #[cfg(target_os = "macos")]
        {
            // On macOS, use /usr/bin/login to ensure proper login shell environment
            // This ensures PATH includes directories like /usr/local/bin
            let shell_name = shell_program.rsplit('/').next().unwrap_or(shell_program);
            let mut login_cmd = Command::new("/usr/bin/login");

            // Check for .hushlogin in home directory
            let hushlogin_path = Path::new(&user.home).join(".hushlogin");

            let flags = if hushlogin_path.exists() {
                "-qflp"
            } else {
                "-flp"
            };

            // -f: Bypasses authentication for already-logged-in user
            // -l: Skips changing directory to $HOME
            // -p: Preserves environment
            // -q: Act as if .hushlogin exists
            login_cmd.args([flags, &user.user]);

            // Build the exec command to replace the intermediate shell with
            // our target shell. Every interpolated value is a shell word in a
            // script zsh parses, so a path with a space, or an argument that
            // is itself a command line, has to arrive quoted rather than be
            // re-split here.
            let mut exec_cmd = format!(
                "exec -a {} {}",
                single_quoted(&format!("-{shell_name}")),
                single_quoted(shell_program)
            );

            for arg in args {
                exec_cmd.push(' ');
                exec_cmd.push_str(&single_quoted(arg));
            }

            // Use /bin/zsh as intermediate shell because it supports 'exec -a'
            login_cmd.args(["/bin/zsh", "-fc", &exec_cmd]);

            login_cmd
        }

        #[cfg(not(target_os = "macos"))]
        {
            let mut cmd = Command::new(shell_program);

            cmd.args(args);

            cmd
        }
    };

    #[cfg(target_os = "linux")]
    {
        // If running inside a flatpak sandbox.
        // Must retrieve $SHELL from outside the sandbox, so ask the host.
        let flatpak_info: PathBuf = "/.flatpak-info".into();

        if flatpak_info.exists() {
            builder = Command::new("flatpak-spawn");

            let mut with_args = vec![
                "--host".to_string(),
                "--watch-bus".to_string(),
                "--env=COLORTERM=truecolor".to_string(),
                "--env=TERM=rio".to_string(),
            ];

            if let Some(directory) = working_directory {
                with_args.push(format!(
                    "--directory={}",
                    path::Path::new(directory).display()
                ));
            }

            let output = Command::new("flatpak-spawn")
                .args(["--host", "sh", "-c", "echo $SHELL"])
                .output()?;

            let shell = String::from_utf8_lossy(&output.stdout);

            with_args.push(shell.trim().to_string());
            with_args.push("-l".to_string());

            builder.args(with_args);

            take_controlling_terminal = false;
        }
    }

    // Setup child stdin/stdout/stderr as child fd of PTY.
    // Ownership of fd is transferred to the Stdio structs and will be closed by them at the end of
    // this scope. (It is not an issue that the fd is closed three times since File::drop ignores
    // error on libc::close.).
    let owned_child = unsafe { OwnedFd::from_raw_fd(child) };

    builder.stdin(owned_child.try_clone()?);
    builder.stderr(owned_child.try_clone()?);
    builder.stdout(owned_child);

    builder.env("USER", user.user);
    builder.env("HOME", user.home);

    // Name the terminal to what runs inside it. Startup files branch on this —
    // macOS `/etc/bashrc` sources `/etc/bashrc_$TERM_PROGRAM` — so inheriting
    // the value of whichever terminal launched the app would attach that
    // terminal's machinery to our sessions. Apple's copy, for one, repoints
    // `HISTFILE` into its own session store.
    builder.env("TERM_PROGRAM", APP_ID);
    builder.env("TERM", terminal_type());

    // Announced rather than inherited for the same reason as `TERM`: the
    // Windows backend declares it on every session it creates, so a Unix child
    // that only sees it when some outer terminal happened to export it would
    // pick a color depth from how the application was started.
    builder.env("COLORTERM", "truecolor");
    builder.envs(environment_overrides.iter().map(|(k, v)| (k, v)));

    unsafe {
        builder.pre_exec(move || prepare_pty_child(child, main, take_controlling_terminal));
    }

    // Handle set working directory option.
    if let Some(dir) = &working_directory {
        builder.current_dir(dir);
    }

    // Prepare signal handling before spawning child.
    let signals = Signals::new([sigconsts::SIGCHLD]).expect("error preparing signal handling");

    match builder.spawn() {
        Ok(child_process) => {
            unsafe {
                set_nonblocking(main);
            }

            let ptsname: String = tty_ptsname(main).unwrap_or_else(|_| "".to_string());

            // `pre_exec` made the child a session leader, so it already leads
            // its own group and attaching only records it.
            let job = manage_process_tree
                .then(|| KillOnCloseJob::attach(&child_process))
                .transpose()?;

            let child_unix = Child {
                id: Arc::new(main),
                ptsname,
                pid: Arc::new(child_process.id().try_into().unwrap()),
                process: Some(child_process),
            };

            Ok(Pty {
                child: child_unix,
                file: unsafe { File::from_raw_fd(main) },
                token: Token(0),
                signals,
                signals_token: Token(0),
                job,
            })
        }

        Err(err) => Err(Error::new(
            err.kind(),
            format!(
                "Failed to spawn command '{}': {}",
                builder.get_program().to_string_lossy(),
                err
            ),
        )),
    }
}

unsafe fn prepare_pty_child(
    child: RawFd,
    main: RawFd,
    take_controlling_terminal: bool,
) -> io::Result<()> {
    unsafe {
        // Create a new process group.
        let err = libc::setsid();

        if err == -1 {
            return Err(Error::last_os_error());
        }

        if take_controlling_terminal {
            set_controlling_terminal(child)?;
        }

        // No longer need child/main fds.
        libc::close(child);
        libc::close(main);

        libc::signal(libc::SIGCHLD, libc::SIG_DFL);
        libc::signal(libc::SIGHUP, libc::SIG_DFL);
        libc::signal(libc::SIGINT, libc::SIG_DFL);
        libc::signal(libc::SIGQUIT, libc::SIG_DFL);
        libc::signal(libc::SIGTERM, libc::SIG_DFL);
        libc::signal(libc::SIGALRM, libc::SIG_DFL);

        Ok(())
    }
}

///
/// Creates a pseudoterminal using fork.
///
/// The [`create_pty`] creates a pseudoterminal with similar behavior as tty,
/// which is a command in Unix and Unix-like operating systems to print the file name of the
/// terminal connected to standard input. tty stands for TeleTYpewriter.
///
/// It returns two [`Pty`] along with respective process name [`String`] and process id (`libc::pid_`)
///
pub fn create_pty_with_fork(
    shell: &str,
    columns: u16,
    rows: u16,
    width: u16,
    height: u16,
) -> Result<Pty, Error> {
    let mut main = 0;

    let winsize = Winsize {
        ws_row: rows as libc::c_ushort,
        ws_col: columns as libc::c_ushort,
        ws_xpixel: width as libc::c_ushort,
        ws_ypixel: height as libc::c_ushort,
    };

    let term = create_termp(true);

    let mut shell_program = shell;

    let user = match ShellUser::from_env() {
        Ok(data) => data,

        Err(..) => ShellUser {
            shell: shell.to_string(),
            ..Default::default()
        },
    };

    if shell.is_empty() {
        info!("shell configuration is empty, will retrieve from env");
        shell_program = &user.shell;
    }

    info!("fork {:?}", shell_program);

    match unsafe {
        forkpty(
            &mut main as *mut _,
            ptr::null_mut(),
            &term as *const libc::termios,
            &winsize as *const _,
        )
    } {
        0 => {
            default_shell_command(shell_program);

            Err(Error::other(format!(
                "forkpty has reach unreachable with {shell_program}"
            )))
        }

        id if id > 0 => {
            // TODO: Currently we fork the process and don't wait to know if led to failure
            // Whenever it happens it will just simply shut down the teletyperwriter
            // In the future add an option to check before release the method
            let ptsname: String = tty_ptsname(main).unwrap_or_else(|_| "".to_string());

            let child = Child {
                id: Arc::new(main),
                ptsname,
                pid: Arc::new(id),
                process: None,
            };

            unsafe {
                set_nonblocking(main);
            }

            let signals =
                Signals::new([sigconsts::SIGCHLD]).expect("error preparing signal handling");

            Ok(Pty {
                child,
                signals,
                file: unsafe { File::from_raw_fd(main) },
                token: Token(0),
                signals_token: Token(0),
                // `forkpty` leaves no `std::process::Child` to attach to, so
                // this path never manages the descendant tree.
                job: None,
            })
        }

        _ => Err(Error::other(format!(
            "forkpty failed using {shell_program}"
        ))),
    }
}

/// Really only needed on BSD, but should be fine elsewhere.
fn set_controlling_terminal(fd: libc::c_int) -> Result<(), Error> {
    let res = unsafe {
        // TIOSCTTY changes based on platform and the `ioctl` call is different
        // based on architecture (32/64). So a generic cast is used to make sure
        // there are no issues. To allow such a generic cast the clippy warning
        // is disabled.
        #[allow(clippy::cast_lossless)]
        libc::ioctl(fd, libc::TIOCSCTTY as _, 0)
    };

    if res < 0 {
        return Err(Error::last_os_error());
    }

    Ok(())
}

// https://man7.org/linux/man-pages/man2/fcntl.2.html
unsafe fn set_nonblocking(fd: libc::c_int) {
    use libc::{F_GETFL, F_SETFL, O_NONBLOCK, fcntl};

    // SAFETY: the caller guarantees `fd` is a live descriptor this owns.
    let res = unsafe { fcntl(fd, F_SETFL, fcntl(fd, F_GETFL, 0) | O_NONBLOCK) };

    assert_eq!(res, 0);
}

#[derive(Debug)]
pub struct Child {
    pub id: Arc<libc::c_int>,
    pub pid: Arc<libc::pid_t>,
    #[allow(dead_code)]
    ptsname: String,
    #[allow(dead_code)]
    process: Option<ChildProcess>,
}

impl Child {
    /// The tcgetwinsize function fills in the winsize structure pointed to by
    ///  gws with values that represent the size of the terminal window for which
    ///  fd provides an open file descriptor.  If no error occurs tcgetwinsize()
    ///  returns zero (0).
    ///  The tcsetwinsize function sets the terminal window size, for the terminal
    ///  referenced by fd, to the sizes from the winsize structure pointed to by
    ///  sws.  If no error occurs tcsetwinsize() returns zero (0).
    ///  The winsize structure, defined in <termios.h>, contains (at least) the
    ///  following four fields
    ///  unsigned short ws_row;      /* Number of rows, in characters */
    ///  unsigned short ws_col;      /* Number of columns, in characters */
    ///  unsigned short ws_xpixel;   /* Width, in pixels */
    ///  unsigned short ws_ypixel;   /* Height, in pixels */
    /// If the actual window size of the controlling terminal of a process
    /// changes, the process is sent a SIGWINCH signal.  See signal(7).  Note
    /// simply changing the sizes using tcsetwinsize() does not necessarily
    /// change the actual window size, and if not, will not generate a SIGWINCH.
    pub fn set_winsize(&self, winsize_builder: WinsizeBuilder) -> io::Result<()> {
        let winsize: Winsize = (&winsize_builder).into();

        match unsafe { libc::ioctl(**self, TIOCSWINSZ, &winsize as *const _) } {
            -1 => Err(io::Error::last_os_error()),
            _ => Ok(()),
        }
    }

    /// Return the child’s exit status if it has already exited. If the child is still running, return Ok(None).
    /// https://linux.die.net/man/2/waitpid
    pub fn waitpid(&self) -> Result<Option<i32>, String> {
        let mut status = 0 as libc::c_int;

        // If WNOHANG was specified in options and there were no children in a waitable state, then waitid() returns 0 immediately and the state of the siginfo_t structure pointed to by infop is unspecified. To distinguish this case from that where a child was in a waitable state, zero out the si_pid field before the call and check for a nonzero value in this field after the call returns.
        let res = unsafe { waitpid(*self.pid, &mut status as *mut libc::c_int, libc::WNOHANG) };

        if res <= -1 {
            return Err("error".into());
        }

        if res == 0 && status == 0 {
            return Ok(None);
        }

        Ok(Some(status))
    }
}

pub fn kill_pid(pid: i32) {
    unsafe {
        libc::kill(pid, libc::SIGHUP);
    }
}

impl Deref for Child {
    type Target = libc::c_int;

    fn deref(&self) -> &libc::c_int {
        &self.id
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        unsafe {
            libc::kill(*self.pid, libc::SIGHUP);
        }
    }
}

pub fn command_per_pid(pid: libc::pid_t) -> String {
    let current_process_name = Command::new("ps")
        .arg("-p")
        .arg(format!("{pid:}"))
        .arg("-o")
        .arg("comm=")
        .output()
        .expect("failed to execute process")
        .stdout;

    str::from_utf8(&current_process_name)
        .unwrap_or("")
        .to_string()
}

impl EventedPty for Pty {
    #[inline]
    fn next_child_event(&mut self) -> Option<ChildEvent> {
        self.signals.pending().next().and_then(|signal| {
            if signal != sigconsts::SIGCHLD {
                return None;
            }

            match self.child.waitpid() {
                Err(_e) => {
                    // std::process::exit(1);
                    None
                }

                Ok(None) => None,
                Ok(Some(..)) => Some(ChildEvent::Exited),
            }
        })
    }

    #[inline]
    fn child_event_token(&self) -> Token {
        self.signals_token
    }
}

#[derive(Debug)]
struct Passwd<'a> {
    name: &'a str,
    dir: &'a str,
    shell: &'a str,
}

/// Return a Passwd struct with pointers into the provided buf.
///
/// # Unsafety
///
/// If `buf` is changed while `Passwd` is alive, bad thing will almost certainly happen.
fn get_pw_entry(buf: &mut [i8; 1024]) -> Result<Passwd<'_>, Error> {
    // Create zeroed passwd struct.
    let mut entry: MaybeUninit<libc::passwd> = MaybeUninit::uninit();

    let mut res: *mut libc::passwd = ptr::null_mut();

    // Try and read the pw file.
    let uid = unsafe { libc::getuid() };

    let status = unsafe {
        libc::getpwuid_r(
            uid,
            entry.as_mut_ptr(),
            buf.as_mut_ptr() as *mut _,
            buf.len(),
            &mut res,
        )
    };

    let entry = unsafe { entry.assume_init() };

    if status < 0 {
        return Err(Error::other("getpwuid_r failed"));
    }

    if res.is_null() {
        return Err(Error::other("pw not found"));
    }

    // Sanity check.
    assert_eq!(entry.pw_uid, uid);

    // Build a borrowed Passwd struct.
    Ok(Passwd {
        name: unsafe { CStr::from_ptr(entry.pw_name).to_str().unwrap() },
        dir: unsafe { CStr::from_ptr(entry.pw_dir).to_str().unwrap() },
        shell: unsafe { CStr::from_ptr(entry.pw_shell).to_str().unwrap() },
    })
}

/// Unsafe
/// Return tty pts name [`String`]
///
/// # Safety
///
/// This function is unsafe because it contains the usage of `libc::ptsname`
/// from libc that's naturally unsafe.
pub fn tty_ptsname(fd: libc::c_int) -> Result<String, String> {
    let c_str: &CStr = unsafe {
        let name_ptr = ptsname(fd as *mut _);

        CStr::from_ptr(name_ptr)
    };

    let str_slice: &str = c_str.to_str().unwrap();
    let str_buf: String = str_slice.to_owned();

    Ok(str_buf)
}

pub fn foreground_process_name(main_fd: RawFd, shell_pid: u32) -> String {
    let mut pid = unsafe { libc::tcgetpgrp(main_fd) };

    if pid < 0 {
        pid = shell_pid as libc::pid_t;
    }

    #[cfg(not(any(target_os = "macos", target_os = "freebsd")))]
    let comm_path = format!("/proc/{pid}/comm");

    #[cfg(target_os = "freebsd")]
    let comm_path = format!("/compat/linux/proc/{pid}/comm");

    #[cfg(not(target_os = "macos"))]
    let name = match fs::read(comm_path) {
        Ok(comm_str) => String::from_utf8_lossy(&comm_str)
            .trim_end()
            .parse()
            .unwrap_or_default(),

        Err(..) => "".into(),
    };

    #[cfg(target_os = "macos")]
    let name = macos_process_name(pid);

    name
}

pub fn foreground_process_path(
    main_fd: RawFd,
    shell_pid: u32,
) -> Result<PathBuf, Box<dyn error::Error>> {
    let mut pid = unsafe { libc::tcgetpgrp(main_fd) };

    if pid < 0 {
        pid = shell_pid as libc::pid_t;
    }

    #[cfg(not(any(target_os = "macos", target_os = "freebsd")))]
    let link_path = format!("/proc/{pid}/cwd");

    #[cfg(target_os = "freebsd")]
    let link_path = format!("/compat/linux/proc/{pid}/cwd");

    #[cfg(not(target_os = "macos"))]
    let cwd = fs::read_link(link_path)?;

    #[cfg(target_os = "macos")]
    let cwd = macos_cwd(pid)?;

    Ok(cwd)
}

/// Start a new process in the background.
pub fn spawn_daemon<I, S>(program: &str, args: I, main_fd: RawFd, shell_pid: u32) -> io::Result<()>
where
    I: IntoIterator<Item = S> + Copy,
    S: AsRef<OsStr>,
{
    let mut command = Command::new(program);

    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    if let Ok(cwd) = foreground_process_path(main_fd, shell_pid) {
        command.current_dir(cwd);
    }

    unsafe {
        command
            .pre_exec(|| {
                match libc::fork() {
                    -1 => return Err(io::Error::last_os_error()),
                    0 => (),
                    _ => libc::_exit(0),
                }

                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }

                Ok(())
            })
            .spawn()?
            .wait()
            .map(|_| ())
    }
}
