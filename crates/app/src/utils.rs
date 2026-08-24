use std::env;
use std::path::PathBuf;

use nmt_platform::windows::environment::data_dir;

/// The NiumaTerm per-user data directory: `%LOCALAPPDATA%\NiumaTerm`, falling back to
/// `%TEMP%` if `LOCALAPPDATA` is unset or uncreatable.
pub(crate) fn get_data_dir() -> PathBuf {
    data_dir()
}

pub(crate) fn get_exe_dir() -> PathBuf {
    env::current_exe()
        .expect("locate current executable")
        .parent()
        .expect("current executable has no parent")
        .to_path_buf()
}
