//! Obtain `Sparkle.framework` and put it where every kind of build can resolve
//! it at run time.
//!
//! The framework's install name is `@rpath/Sparkle.framework/Versions/B/Sparkle`,
//! so the loader finds it through whatever rpaths the executable carries. A
//! packaged application carries `@executable_path/../Frameworks`; a binary run
//! straight out of `target/` carries `@executable_path`, which is why a copy
//! lands beside the executables here rather than only inside a bundle.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::{env, fs};

/// Pinned so a build cannot pick up a different framework than the one the
/// release job signs and notarizes, and checksummed because the archive is
/// fetched over the network from a third party.
const SPARKLE_VERSION: &str = "2.9.6";

const SPARKLE_SHA256: &str = "52bf9e88cdd972fc0c81501377a880e90d47031bd8ca5462488f843e2609e192";

/// Names a directory that already holds `Sparkle.framework`, for a build with
/// no network access or one that wants to share a single download.
const FRAMEWORK_DIR_ENV: &str = "NMT_SPARKLE_FRAMEWORK_DIR";

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed={FRAMEWORK_DIR_ENV}");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }

    let out_dir: PathBuf = env::var("OUT_DIR").unwrap().into();

    let source = match env::var_os(FRAMEWORK_DIR_ENV) {
        Some(dir) => dir.into(),
        None => fetch_framework(&out_dir),
    };

    let framework = source.join("Sparkle.framework");

    assert!(
        framework.is_dir(),
        "no Sparkle.framework in {}",
        source.display()
    );

    // OUT_DIR is <target>/<profile>/build/nmt_sparkle-<hash>/out; walk up to
    // <target>/<profile>, which holds the binaries and, in deps/, the test
    // executables.
    let profile_dir = out_dir
        .ancestors()
        .nth(3)
        .expect("OUT_DIR is not under a target profile directory")
        .to_path_buf();

    for dir in [profile_dir.clone(), profile_dir.join("deps")] {
        install(&framework, &dir);
    }

    println!(
        "cargo:rustc-link-search=framework={}",
        profile_dir.display()
    );

    println!("cargo:rustc-link-lib=framework=Sparkle");

    // The application crate emits the rpaths a packaged app needs, since it is
    // the one that knows the bundle layout. This crate's own test executables
    // run out of deps/, where the copy above sits.
    println!("cargo:rustc-link-arg=-Wl,-rpath,@executable_path");
}

/// Download and unpack the pinned release, reusing an earlier unpack.
fn fetch_framework(out_dir: &Path) -> PathBuf {
    let unpacked = out_dir.join(format!("sparkle-{SPARKLE_VERSION}"));

    if unpacked.join("Sparkle.framework").is_dir() {
        return unpacked;
    }

    let archive = out_dir.join(format!("Sparkle-{SPARKLE_VERSION}.tar.xz"));

    if !archive.is_file() {
        let url = format!(
            "https://github.com/sparkle-project/Sparkle/releases/download/{SPARKLE_VERSION}/Sparkle-{SPARKLE_VERSION}.tar.xz"
        );

        let mut curl = Command::new("curl");

        curl.args(["-fsSL", "-o"]).arg(&archive).arg(&url);
        run(curl, "download Sparkle");
    }

    verify_checksum(&archive);

    fs::create_dir_all(&unpacked).expect("create the unpack directory");

    let mut tar = Command::new("tar");

    tar.arg("-xJf").arg(&archive).arg("-C").arg(&unpacked);
    run(tar, "unpack Sparkle");

    unpacked
}

/// Reject an archive whose bytes are not the pinned ones, and delete it so the
/// next build refetches rather than failing the same way forever.
fn verify_checksum(archive: &Path) {
    let output = Command::new("shasum")
        .args(["-a", "256"])
        .arg(archive)
        .output()
        .expect("run shasum");

    assert!(
        output.status.success(),
        "shasum failed on {}",
        archive.display()
    );

    let stdout = String::from_utf8(output.stdout).expect("shasum printed non-UTF-8");

    let digest = stdout
        .split_whitespace()
        .next()
        .expect("shasum printed no digest");

    if digest != SPARKLE_SHA256 {
        let _ = fs::remove_file(archive);

        panic!("Sparkle {SPARKLE_VERSION} archive is {digest}, expected {SPARKLE_SHA256}");
    }
}

/// Copy the framework into `dir`, replacing any earlier copy.
///
/// `ditto` rather than a recursive file copy: a framework's `Versions/Current`
/// symlinks are part of what its code signature seals, and a copy that resolves
/// them produces a bundle the loader rejects.
fn install(framework: &Path, dir: &Path) {
    if let Err(e) = fs::create_dir_all(dir) {
        println!("cargo:warning=could not create {}: {e}", dir.display());

        return;
    }

    let destination = dir.join("Sparkle.framework");
    let mut ditto = Command::new("ditto");

    ditto.arg(framework).arg(&destination);

    match ditto.status() {
        Ok(status) if status.success() => {}

        // A running application can hold the framework open. Warn rather than
        // fail: the copy already in place is the same pinned version.
        Ok(status) => println!(
            "cargo:warning=ditto into {} exited with {status}",
            destination.display()
        ),

        Err(e) => println!(
            "cargo:warning=could not run ditto into {}: {e}",
            destination.display()
        ),
    }
}

fn run(mut command: Command, context: &str) {
    let status = command
        .status()
        .unwrap_or_else(|e| panic!("could not {context}: {e}"));

    assert!(
        status.success(),
        "could not {context}: exited with {status}"
    );
}
