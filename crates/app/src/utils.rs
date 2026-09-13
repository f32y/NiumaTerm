use std::env;
use std::path::PathBuf;

pub fn get_exe_dir() -> PathBuf {
    env::current_exe()
        .expect("locate current executable")
        .parent()
        .expect("current executable has no parent")
        .to_path_buf()
}
