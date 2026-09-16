//! Application updates without a dependency on the application's view framework.
//!
//! Windows exposes blocking jobs and their outcomes so the host can schedule
//! work, obtain user decisions, and quit after a successful relaunch. macOS
//! delegates package replacement and its native prompts to Sparkle.

#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(windows)]
pub mod windows;

/// The successor waits for the replaced process before claiming the
/// single-instance mutex, which remains held until the predecessor exits.
pub const AWAIT_EXIT_FLAG: &str = "--await-exit";
