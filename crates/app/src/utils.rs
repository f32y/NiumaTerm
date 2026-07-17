use std::path::{Path, PathBuf};

/// The NiumaTerm per-user data directory: `%LOCALAPPDATA%\NiumaTerm`, falling back to
/// `%TEMP%` if `LOCALAPPDATA` is unset or uncreatable.
pub(crate) fn get_data_dir() -> PathBuf {
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        let dir = Path::new(&local).join("NiumaTerm");
        if std::fs::create_dir_all(&dir).is_ok() {
            return dir;
        }
    }
    std::env::temp_dir()
}

pub(crate) fn get_exe_dir() -> PathBuf {
    std::env::current_exe()
        .expect("locate current executable")
        .parent()
        .expect("current executable has no parent")
        .to_path_buf()
}

pub const POWERSHELL_INTEGRATION: &str =
    include_str!("../../../assets/windows/pwsh-integration.ps1");
