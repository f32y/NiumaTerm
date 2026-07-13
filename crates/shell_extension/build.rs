use std::env;
use std::path::PathBuf;

fn main() {
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_os != "windows" || target_env != "msvc" {
        return;
    }

    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let def = manifest_dir.join("shell_extension.def");
    println!("cargo:rerun-if-changed={}", def.display());
    println!("cargo:rustc-link-arg-cdylib=/DEF:{}", def.display());

    winres::WindowsResource::new()
        .set("FileDescription", "NiumaTerm Shell Extension")
        .set("ProductName", "NiumaTerm")
        .set("InternalName", "NiumaTerm Shell Extension")
        .set("OriginalFilename", "shell_extension.dll")
        .compile()
        .unwrap();
}
