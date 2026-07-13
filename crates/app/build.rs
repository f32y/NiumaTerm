use std::path::PathBuf;

fn main() {
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        let icon = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/windows/app.ico");
        println!("cargo:rerun-if-changed={}", icon.display());
        winres::WindowsResource::new()
            .set_icon(icon.to_str().unwrap())
            .set("FileDescription", "NiumaTerm")
            .set("ProductName", "NiumaTerm")
            .set("InternalName", "NiumaTerm")
            .set("OriginalFilename", "NiumaTerm.exe")
            .compile()
            .unwrap();
    }

    let src =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/windows/pwsh-integration.ps1");
    println!("cargo:rerun-if-changed={}", src.display());

    // OUT_DIR is target/<profile>/build/<pkg>-<hash>/out — binary dir is 3 levels up.
    let bin_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap())
        .ancestors()
        .nth(3)
        .unwrap()
        .to_path_buf();
    let dst_dir = bin_dir.join("assets");
    std::fs::create_dir_all(&dst_dir).unwrap();
    std::fs::copy(&src, dst_dir.join("pwsh-integration.ps1")).unwrap();
}
