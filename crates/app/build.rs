use std::env;
use std::path::PathBuf;

use winres::WindowsResource;

fn main() {
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
