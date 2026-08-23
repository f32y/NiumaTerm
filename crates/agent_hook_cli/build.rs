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
        .set("FileDescription", "NiumaTerm Agent Hook")
        .set("ProductName", "NiumaTerm")
        .set("InternalName", "NiumaTerm Agent Hook")
        .set("OriginalFilename", "NmtAgentHook.exe")
        // Agent configurations point at this executable by absolute path, so a
        // copy can outlive the installation that wrote it; the label identifies
        // which one a stray copy came from.
        .set("FileVersion", &version)
        .set("ProductVersion", &version)
        .compile()
        .unwrap();
}
