use std::{env, process};

use crate::unix::macos::*;

#[test]
fn cwd_matches_current_dir() {
    assert_eq!(
        macos_cwd(process::id() as i32).ok(),
        env::current_dir().ok()
    );
}
