//! The build-script half: producing a label, and naming the revision a crate
//! last changed in. Both shell out to git and abort the build when they cannot
//! answer, so they belong to build scripts alone.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::Version;

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
                Version::parse(&label).is_some(),
                "NIUMATERM_VERSION `{label}` is not {VERSION_FORMS}"
            );
            label
        }
        None => derive_from_git(),
    };

    println!("cargo:rustc-env=NIUMATERM_VERSION={version}");

    version
}

/// Emits `NIUMATERM_INTERNAL_VERSION` for `env!` in the calling crate: the
/// branch and abbreviated revision the build came from. The release label
/// cannot answer which branch a build was cut from, and a nightly label drops
/// the branch as well, so a bug report from a side branch is otherwise
/// indistinguishable from one built on `main`. Call from a build script only.
pub fn emit_internal() {
    if let Some(git_dir) = git_dir() {
        // Committing on the current branch rewrites the branch ref rather than
        // `HEAD`, which keeps naming the same branch; the reflog is the one
        // file both a commit and a checkout always append to.
        println!(
            "cargo:rerun-if-changed={}",
            git_dir.join("logs").join("HEAD").display()
        );
    }

    let internal = match (
        run_git(&["rev-parse", "--abbrev-ref", "HEAD"]),
        run_git(&["rev-parse", "--short=7", "HEAD"]),
    ) {
        (Some(branch), Some(commit)) => format!("{branch}@{commit}"),
        // A build from a published archive has no checkout to read, and the
        // label is diagnostic rather than load-bearing, so it degrades instead
        // of failing the build the way a missing version label does.
        _ => "unknown".to_owned(),
    };

    println!("cargo:rustc-env=NIUMATERM_INTERNAL_VERSION={internal}");
}

/// The revision that last changed the calling crate, for a binary an update
/// replaces only when it moved forward rather than on every release.
///
/// Explorer keeps a registered shell extension mapped in its own process, so
/// replacing that file costs a stale context menu until Explorer unloads it.
/// The release name cannot decide whether that cost is worth paying, because it
/// advances on every build while the same extension stays correct across
/// releases that did not touch it.
///
/// This reads committed history, so an uncommitted edit is invisible to it.
/// That suits a value only published packages are compared by, and it means a
/// shallow checkout with no history fails the build instead of quietly naming
/// the wrong revision.
pub fn crate_revision() -> String {
    // A build script runs in its own package root, which is the directory this
    // asks about; naming it instead would be a repository-relative path the
    // caller has no other reason to know.
    run_git(&["log", "-1", "--format=%h", "--abbrev=7", "--", "."]).unwrap_or_else(|| {
        panic!(
            "no committed revision for this crate; the build needs a checkout \
             with history, not a shallow one"
        )
    })
}

fn derive_from_git() -> String {
    // `describe` reports whichever tag it considers newest, so a revision
    // carrying both a release tag and an unrelated one could hide the release.
    if let Some(tag) = run_git(&["tag", "--points-at", "HEAD"]).and_then(|tags| {
        tags.lines()
            .map(str::trim)
            .find(|tag| matches!(Version::parse(tag), Some(Version::Release { .. })))
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
        matches!(Version::parse(&label), Some(Version::Nightly { .. })),
        "derived version `{label}` is not {VERSION_FORMS}"
    );

    label
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
