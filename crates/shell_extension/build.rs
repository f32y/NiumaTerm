use std::env;
use std::path::PathBuf;

use winres::WindowsResource;

fn main() {
    let version = nmt_build_version::emit();

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_os != "windows" || target_env != "msvc" {
        return;
    }

    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let def = manifest_dir.join("shell_extension.def");
    println!("cargo:rerun-if-changed={}", def.display());
    println!("cargo:rustc-link-arg-cdylib=/DEF:{}", def.display());

    let revision = nmt_build_version::crate_revision();

    WindowsResource::new()
        .set("FileDescription", "NiumaTerm Shell Extension")
        .set("ProductName", "NiumaTerm")
        .set("InternalName", "NiumaTerm Shell Extension")
        .set("OriginalFilename", "shell_extension.dll")
        .set("FileVersion", &version)
        .set("ProductVersion", &version)
        // Explorer keeps a registered extension loaded, so an update replaces
        // this file only when the extension itself moved forward. The release
        // name above cannot express that: it advances on every build, while the
        // same DLL stays correct across releases that did not touch it.
        .set("InternalVersion", &revision)
        .compile()
        .unwrap();
}
