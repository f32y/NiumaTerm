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
// the release or nightly name it publishes under and passes it in. A local build
// has no such name, so it reuses the tag on HEAD when there is one, which is the
// same name the workflow would publish that revision under, and otherwise the
// HEAD commit, the only thing that distinguishes one working copy from another
// between version bumps.
fn emit_version() {
    println!("cargo:rerun-if-env-changed=NIUMATERM_VERSION");
    // HEAD covers moving to another revision; the tag directory covers tagging
    // the revision already checked out, which is how a release is cut.
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs/tags");

    let version = env::var("NIUMATERM_VERSION")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| git_describe(&["describe", "--tags", "--exact-match", "HEAD"]))
        .or_else(|| git_describe(&["rev-parse", "--short=7", "HEAD"]))
        .unwrap_or_else(|| env::var("CARGO_PKG_VERSION").unwrap());

    println!("cargo:rustc-env=NIUMATERM_VERSION={version}");
}

fn git_describe(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;

    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|name| !name.is_empty())
}
