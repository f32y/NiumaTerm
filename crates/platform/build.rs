//! Copy the bundled Windows Terminal ConPTY (`conpty.dll` + `OpenConsole.exe`) next to
//! the built executable. `conpty.rs` loads them from the executable's directory at
//! startup and requires them: the in-box system ConPTY repaints the whole buffer on
//! resize and corrupts scrollback, while the bundled WT ConPTY implements the
//! no-repaint resize quirk.
use std::path::PathBuf;

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    // crates/platform -> repo root -> assets/windows
    let src_dir = manifest
        .join("..")
        .join("..")
        .join("assets")
        .join("windows");
    // OUT_DIR = <target>/<profile>/build/nmt_platform-<hash>/out — walk up 3 to <target>/<profile>.
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let Some(profile_dir) = out_dir.ancestors().nth(3) else {
        println!("cargo:warning=could not derive target profile dir from OUT_DIR");
        return;
    };

    for name in ["conpty.dll", "OpenConsole.exe"] {
        let from = src_dir.join(name);
        println!("cargo:rerun-if-changed={}", from.display());
        for dir in [profile_dir.to_path_buf(), profile_dir.join("deps")] {
            let to = dir.join(name);
            if let Err(e) = std::fs::copy(&from, &to) {
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
