use std::path::{Path, PathBuf};

use crate::links::resolve_local_path;

#[test]
fn resolves_agent_file_locations() {
    let cwd = Path::new("C:/Workspace/NiumaTerm");

    assert_eq!(
        resolve_local_path("crates/app/src/main.rs:42", Some(cwd)),
        Some(cwd.join("crates/app/src/main.rs"))
    );
    assert_eq!(
        resolve_local_path("C:/Workspace/NiumaTerm/Cargo.toml:582", Some(cwd)),
        Some(PathBuf::from("C:/Workspace/NiumaTerm/Cargo.toml"))
    );
    assert_eq!(
        resolve_local_path("/C:/Workspace/NiumaTerm/Cargo.toml:111", Some(cwd)),
        Some(PathBuf::from("C:/Workspace/NiumaTerm/Cargo.toml"))
    );
    assert_eq!(
        resolve_local_path("crates/app/src/main.rs:42:7", Some(cwd)),
        Some(cwd.join("crates/app/src/main.rs"))
    );
    assert_eq!(
        resolve_local_path("https://example.com/file.rs:42", Some(cwd)),
        None
    );
}
