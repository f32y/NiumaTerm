use std::{env, process};

#[cfg(windows)]
use nmt_remote_session_hub::ipc::run_hub_host;

#[cfg(windows)]
fn main() {
    let Some(os_id) = env::args().nth(1) else {
        eprintln!("usage: SessionHub <shared-memory-id>");
        process::exit(2);
    };
    if let Err(error) = run_hub_host(&os_id) {
        eprintln!("remote session Hub stopped: {error}");
        process::exit(1);
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("SessionHub is only available on Windows");
    process::exit(1);
}
