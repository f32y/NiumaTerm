use std::path::Path;

use gpui::AssetSource as _;

use crate::agent_tab::transcript::render::text_style::file_icons::file_link_icon;
use crate::agent_tab::transcript::render::text_style::resolve_local_path;
use crate::assets::AppAssets;

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

#[test]
fn file_links_choose_embedded_icons_without_accessing_the_target() {
    for (target, expected) in [
        ("C:/missing/Cargo.toml:42:7", "Config-Rust"),
        ("/C:/missing/SOURCE.RS:12", "Config-Rust"),
        ("src\\main.ts", "TypeScript"),
        ("src/中文.py:5", "Config-Python"),
        ("package.json", "NPM"),
        ("data.json:9", "JSON-1"),
        ("config.yaml", "YAML"),
        ("Dockerfile", "Docker"),
        ("README.md", "Default"),
        ("unknown.custom", "Default"),
        ("report.PDF", "Adobe-Acrobat"),
        ("file:///C:/project/my%20file.ts", "TypeScript"),
    ] {
        let icon = file_link_icon(target).expect("local file icon");

        assert_eq!(icon.as_ref(), format!("file-icons/{expected}.svg"));
        assert!(
            AppAssets.load(&icon).expect("asset lookup").is_some(),
            "missing {icon}"
        );
    }

    for target in [
        "https://example.com/file.rs",
        "mailto:a@example.com",
        "#heading",
        "",
        "src/",
        "../",
    ] {
        assert!(
            file_link_icon(target).is_none(),
            "unexpected icon for {target}"
        );
    }
}
