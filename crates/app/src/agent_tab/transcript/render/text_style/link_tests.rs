use std::path::Path;

use crate::agent_tab::transcript::render::text_style::resolve_local_path;

#[test]
fn resolves_agent_file_locations() {
    let cwd = Path::new("C:/Workspace/NiumaTerm");

    assert_eq!(
        resolve_local_path("crates/app/src/main.rs:42", Some(cwd)),
        Some(cwd.join("crates/app/src/main.rs"))
    );
    assert_eq!(
        resolve_local_path("C:/Workspace/NiumaTerm/Cargo.toml:582", Some(cwd)),
        Some("C:/Workspace/NiumaTerm/Cargo.toml".into())
    );
    assert_eq!(
        resolve_local_path("/C:/Workspace/NiumaTerm/Cargo.toml:111", Some(cwd)),
        Some("C:/Workspace/NiumaTerm/Cargo.toml".into())
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
