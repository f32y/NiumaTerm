use std::env;

use winres::WindowsResource;

fn main() {
    let version = nmt_version::emit();

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_os != "windows" || target_env != "msvc" {
        return;
    }

    WindowsResource::new()
        .set("FileDescription", "NiumaTerm Tree-sitter Languages")
        .set("ProductName", "NiumaTerm")
        .set("InternalName", "NiumaTerm Tree-sitter Languages")
        .set("OriginalFilename", "tree_sitter.dll")
        .set("FileVersion", &version)
        .set("ProductVersion", &version)
        .compile()
        .unwrap();
}
