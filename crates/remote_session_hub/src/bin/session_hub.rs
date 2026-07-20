#[cfg(windows)]
fn main() {
    let Some(os_id) = std::env::args().nth(1) else {
        eprintln!("usage: SessionHub <shared-memory-id>");
        std::process::exit(2);
    };
    if let Err(error) = nmt_remote_session_hub::ipc::run_hub_host(&os_id) {
        eprintln!("remote session Hub stopped: {error}");
        std::process::exit(1);
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("SessionHub is only available on Windows");
    std::process::exit(1);
}
