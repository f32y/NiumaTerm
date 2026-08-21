use std::env;
use std::path::PathBuf;

use winres::WindowsResource;

fn main() {
    let version = nmt_build_version::emit();

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
}
