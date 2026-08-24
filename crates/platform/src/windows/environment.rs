use std::path::{Path, PathBuf};
use std::{env, fs};

use crate::APP_ID;

pub fn data_dir() -> PathBuf {
    if let Some(local) = env::var_os("LOCALAPPDATA") {
        let directory = Path::new(&local).join(APP_ID);
        if fs::create_dir_all(&directory).is_ok() {
            return directory;
        }
    }
    env::temp_dir()
}

pub fn home_dir() -> Option<PathBuf> {
    env::var_os("USERPROFILE")
        .or_else(|| env::var_os("HOME"))
        .map(PathBuf::from)
}

pub fn config_dir(home: &Path) -> PathBuf {
    home.join("AppData").join("Local").join(APP_ID)
}

pub fn computer_name() -> Option<String> {
    env::var("COMPUTERNAME")
        .ok()
        .filter(|name| !name.is_empty())
}

pub const DEFAULT_EDITOR: &str = "notepad";
