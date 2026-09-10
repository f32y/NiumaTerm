use std::env;
use std::path::PathBuf;

use winres::WindowsResource;

fn main() {
    let version = nmt_version::emit();

    nmt_version::emit_internal();

    if env::var_os("CARGO_CFG_WINDOWS").is_some() {
        let icon = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/windows/app.ico");

        println!("cargo:rerun-if-changed={}", icon.display());
        WindowsResource::new()
            .set_icon(icon.to_str().unwrap())
            .set("FileDescription", "NiumaTerm")
            .set("ProductName", "NiumaTerm")
            .set("InternalName", "NiumaTerm")
            .set("OriginalFilename", "NiumaTerm.exe")
            // The release tag verbatim, so the string a user reads in the file
            // properties is the one they can search for on the releases page.
            // The numeric FILEVERSION stays on the crate version, which is the
            // only form Windows accepts there and cannot express a nightly
            // date or a commit.
            .set("FileVersion", &version)
            .set("ProductVersion", &version)
            .compile()
            .unwrap();
    }

    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        emit_framework_rpaths();
    }
}

/// Tell the loader where to find `Sparkle.framework`, whose install name is
/// `@rpath/Sparkle.framework/Versions/B/Sparkle`.
///
/// Two locations, tried in this order. A packaged application keeps the
/// framework in `Contents/Frameworks` beside `Contents/MacOS`. A binary run
/// straight out of `target/` has no bundle around it, and `nmt_sparkle`'s build
/// script leaves a copy next to the executable for exactly that case; without
/// the second entry a development build would fail to launch rather than fail
/// to update.
///
/// Emitted for every link target rather than binaries alone: a test executable
/// links the same dependency graph, so it needs the framework too, and it runs
/// from `deps/`, where the same build script leaves a second copy.
fn emit_framework_rpaths() {
    for path in ["@executable_path/../Frameworks", "@executable_path"] {
        println!("cargo:rustc-link-arg=-Wl,-rpath,{path}");
    }
}
