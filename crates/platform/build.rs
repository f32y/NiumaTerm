//! Copy the bundled Windows Terminal ConPTY (`conpty.dll` + `OpenConsole.exe`) next to
//! the built executable. `conpty.rs` loads them from the executable's directory at
//! startup and requires them: the in-box system ConPTY repaints the whole buffer on
//! resize and corrupts scrollback, while the bundled WT ConPTY implements the
//! no-repaint resize quirk.

use std::path::PathBuf;
use std::{env, fs};

fn main() {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let manifest: PathBuf = env::var("CARGO_MANIFEST_DIR").unwrap().into();

    // crates/platform -> repo root -> assets/windows
    let src_dir = manifest
        .join("..")
        .join("..")
        .join("assets")
        .join("windows");

    let out_dir: PathBuf = env::var("OUT_DIR").unwrap().into();

    // Cargo stores build-script output below the profile's build directory,
    // but the number of package/hash directories differs between versions.
    let Some(profile_dir) = out_dir
        .ancestors()
        .find(|dir| dir.file_name().is_some_and(|name| name == "build"))
        .and_then(|build_dir| build_dir.parent())
    else {
        println!("cargo:warning=could not derive target profile dir from OUT_DIR");

        return;
    };

    // Cargo adds library search paths inside OUT_DIR to the environment of
    // test and example processes, whose executable directories vary by version.
    println!("cargo:rustc-link-search=native={}", out_dir.display());

    for name in ["conpty.dll", "OpenConsole.exe"] {
        let from = src_dir.join(name);

        println!("cargo:rerun-if-changed={}", from.display());

        for dir in [profile_dir, out_dir.as_path()] {
            let to = dir.join(name);

            if let Err(e) = fs::copy(&from, &to) {
                // A running app may hold the file open — warn, don't fail the build.
                println!(
                    "cargo:warning=failed to copy {} -> {}: {e}",
                    from.display(),
                    to.display()
                );
            }
        }
    }
}
