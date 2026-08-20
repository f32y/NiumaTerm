use std::env;
use std::path::PathBuf;
use std::process::Command;

use winres::WindowsResource;

fn main() {
    emit_version();

    if env::var_os("CARGO_CFG_WINDOWS").is_some() {
        let icon = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/windows/app.ico");
        println!("cargo:rerun-if-changed={}", icon.display());
        WindowsResource::new()
            .set_icon(icon.to_str().unwrap())
            .set("FileDescription", "NiumaTerm")
            .set("ProductName", "NiumaTerm")
            .set("InternalName", "NiumaTerm")
            .set("OriginalFilename", "NiumaTerm.exe")
            .compile()
            .unwrap();
    }
}

// The About page shows what a binary was built from. A packaging workflow knows
// the release or nightly name it publishes under and passes it in; a local build
// has no such name, so it falls back to the HEAD commit, which is the only thing
// that distinguishes one working copy from another between version bumps.
fn emit_version() {
    println!("cargo:rerun-if-env-changed=NIUMATERM_VERSION");
    println!("cargo:rerun-if-changed=../../.git/HEAD");

    let version = env::var("NIUMATERM_VERSION")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(head_commit)
        .unwrap_or_else(|| env::var("CARGO_PKG_VERSION").unwrap());

    println!("cargo:rustc-env=NIUMATERM_VERSION={version}");
}

fn head_commit() -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--short=7", "HEAD"])
        .output()
        .ok()?;

    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|hash| !hash.is_empty())
}
