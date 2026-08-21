//! Build-time version label shared by every binary this workspace links.
//!
//! A label has exactly one of two forms, so that every binary in an
//! installation directory, every published asset, and the tag its release was
//! cut from all carry the same string and compare against each other directly:
//!
//! - release: `v1.2.0`, the tag a release build was cut from
//! - nightly: `nightly-20260821-7567b41`, the committer date and revision a
//!   build came from
//!
//! Both the workflow-provided name and the locally derived one are checked
//! against those forms, and a build that can produce neither fails rather than
//! shipping a binary whose version cannot be compared with a release.
//!
//! The nightly date comes from the commit rather than the wall clock, so
//! rebuilding a revision yields the same label the packaging workflow published
//! it under and a locally built binary is not mistaken for an outdated one.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

const VERSION_FORMS: &str =
    "a release version such as `v1.2.0` or a nightly version such as `nightly-20260821-7567b41`";

/// Emits `NIUMATERM_VERSION` for `env!` in the calling crate and returns it for
/// the caller's Windows version resource. Call from a build script only: the
/// `cargo:` directives below are read from the build script's standard output.
pub fn emit() -> String {
    println!("cargo:rerun-if-env-changed=NIUMATERM_VERSION");
    if let Some(git_dir) = git_dir() {
        // HEAD covers moving to another revision; the tag directory covers
        // tagging the revision already checked out, which is how a release is
        // cut.
        println!("cargo:rerun-if-changed={}", git_dir.join("HEAD").display());
        println!(
            "cargo:rerun-if-changed={}",
            git_dir.join("refs").join("tags").display()
        );
    }

    let version = match env::var("NIUMATERM_VERSION")
        .ok()
        .filter(|value| !value.trim().is_empty())
    {
        Some(label) => {
            let label = label.trim().to_owned();
            assert!(
                is_release(&label) || is_nightly(&label),
                "NIUMATERM_VERSION `{label}` is not {VERSION_FORMS}"
            );
            label
        }
        None => derive_from_git(),
    };

    println!("cargo:rustc-env=NIUMATERM_VERSION={version}");

    version
}

fn derive_from_git() -> String {
    // `describe` reports whichever tag it considers newest, so a revision
    // carrying both a release tag and an unrelated one could hide the release.
    if let Some(tag) = run_git(&["tag", "--points-at", "HEAD"]).and_then(|tags| {
        tags.lines()
            .map(str::trim)
            .find(|tag| is_release(tag))
            .map(str::to_owned)
    }) {
        return tag;
    }

    let (Some(date), Some(commit)) = (
        run_git(&["show", "-s", "--format=%cd", "--date=format:%Y%m%d", "HEAD"]),
        run_git(&["rev-parse", "--short=7", "HEAD"]),
    ) else {
        panic!(
            "no release tag on HEAD and no revision to date a nightly from; set NIUMATERM_VERSION to {VERSION_FORMS}"
        );
    };

    let label = format!("nightly-{date}-{commit}");
    assert!(
        is_nightly(&label),
        "derived version `{label}` is not {VERSION_FORMS}"
    );

    label
}

/// A release version: `v` followed by three dot-separated numbers.
fn is_release(label: &str) -> bool {
    let Some(numbers) = label.strip_prefix('v') else {
        return false;
    };
    let mut parts = numbers.split('.');
    let leading = parts
        .by_ref()
        .take(3)
        .filter(|part| is_number(part))
        .count();

    leading == 3 && parts.next().is_none()
}

/// A nightly version: an eight-digit date and an abbreviated commit.
fn is_nightly(label: &str) -> bool {
    let Some(rest) = label.strip_prefix("nightly-") else {
        return false;
    };
    let Some((date, commit)) = rest.split_once('-') else {
        return false;
    };

    date.len() == 8
        && is_number(date)
        && commit.len() >= 7
        && commit.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn is_number(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
}

/// Walking up from the package instead of assuming a fixed depth keeps the
/// rebuild triggers working for a crate that moves within the workspace, and
/// yields nothing for a build from a published archive, where the triggers
/// would have no revision to watch anyway.
fn git_dir() -> Option<PathBuf> {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR")?);
    manifest
        .ancestors()
        .map(|directory| directory.join(".git"))
        .find(|candidate| Path::is_dir(candidate))
}

fn run_git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;

    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests;
