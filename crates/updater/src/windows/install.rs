//! Replacing the installed files with a staged release, in place.
//!
//! Windows refuses to delete a file that is mapped as a running image but
//! allows renaming one, which is what makes an installation replace itself
//! without a second program to do it: every file moves aside under a new name
//! and the staged copy takes the name it vacated. A process that has one of the
//! old files mapped — this one, and Explorer for the context-menu extension —
//! keeps running from the renamed file until it exits.

#[cfg(test)]
#[path = "install_tests.rs"]
mod install_tests;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use std::{fs, process};

use tracing::warn;

use crate::AWAIT_EXIT_FLAG;
use crate::windows::file_version::version_string;
use crate::windows::releases::Release;
use crate::windows::self_update::{ReplaceFilesError, discard_previous, replace_files};

/// Why an update that was found could not be put in place. Each variant names
/// the step that refused, because what a user can do about it differs: a
/// release with nothing attached to it is not the same problem as an
/// installation directory this account may not write to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallError {
    /// The release has no package, or none published beside a checksum.
    NoPackage,
    Unreachable,
    /// What arrived is not what was published.
    Checksum,
    Unpack,
    NotWritable,
    Replace,
    /// The files were replaced, so the update did land; only the restart into
    /// it did not.
    Relaunch,
}

const APP_EXE: &str = "NiumaTerm.exe";
pub(super) const SHELL_EXTENSION_DLL: &str = "NmtShellExtension.dll";

/// A downloaded release and the exact files selected for replacement.
/// Paths are captured before replacement can rename the running executable.
pub struct Installation {
    release: Release,
    staged: PathBuf,
    install: PathBuf,
    plan: InstallPlan,
    testing: bool,
}

impl Installation {
    pub(super) fn new(release: Release, staged: PathBuf, install: PathBuf, testing: bool) -> Self {
        let plan = plan(&staged, &install);

        if plan.is_empty() {
            warn!("update: the published package matches what is installed");
        }

        Self {
            release,
            staged,
            install,
            plan,
            testing,
        }
    }

    pub(super) fn release(&self) -> &Release {
        &self.release
    }

    pub(super) fn changes_shell_extension(&self) -> bool {
        self.plan.contains(SHELL_EXTENSION_DLL)
    }

    pub(super) fn shell_extension(&self) -> PathBuf {
        self.install.join(SHELL_EXTENSION_DLL)
    }

    pub(super) fn apply(&self) -> Result<(), InstallError> {
        apply(&self.staged, &self.install, &self.plan)
    }

    pub(super) fn relaunch(&self) -> Result<(), InstallError> {
        relaunch(&self.install, self.testing)
    }
}

/// Install the files `package` carries and `install` does not have.
///
/// A swap copies what the build performing it knows to look for, and the build
/// performing it is the one being replaced. A release published before this one
/// installed its executable without the files it had never heard of, so the
/// first start of the new build is the first moment something that knows about
/// them is running, and the package they arrived in is still staged.
///
/// A file that is already installed is left alone: this restores what an update
/// skipped, and deciding which installed files a package supersedes belongs to
/// the swap, which can rename a mapped file aside where a plain copy over one
/// would be refused.
pub(super) fn install_additions(package: &Path, install: &Path) {
    if !carries_installed_app(package, install) {
        return;
    }

    for name in staged_names(package) {
        let target = install.join(&name);

        if target.exists() {
            continue;
        }

        if let Err(error) = fs::copy(package.join(&name), &target) {
            warn!("update: installing the missing {name} failed: {error}");
        }
    }
}

/// Whether `package` is the release the installed executable came from.
///
/// Staging can also hold an attempt that never replaced anything, and a file
/// taken out of a different release would pair a grammar or a helper with an
/// executable that never shipped beside it. A package whose executable carries
/// no readable version cannot be attributed to a release at all, so it is not
/// treated as a match for an installation that reads as unversioned either.
fn carries_installed_app(package: &Path, install: &Path) -> bool {
    let staged = version_string(&package.join(APP_EXE), "FileVersion");

    staged.is_some() && staged == version_string(&install.join(APP_EXE), "FileVersion")
}

/// How long a restarting instance waits for its predecessor before starting
/// anyway. Waiting at all is what keeps it from reaching the single-instance
/// check while the old process still holds the mutex; waiting forever would
/// hand a shutdown that never finishes the power to prevent the restart.
pub(super) const PREDECESSOR_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, PartialEq, Eq)]
struct InstallPlan {
    names: Vec<String>,
}

impl InstallPlan {
    fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    fn contains(&self, name: &str) -> bool {
        self.names.iter().any(|staged| staged == name)
    }
}

/// Select the staged files that differ from the installed copies.
///
/// Capturing this before any file moves keeps later pre-install decisions tied
/// to the exact set that the swap will consume.
fn plan(staging: &Path, install: &Path) -> InstallPlan {
    let versions = versions(staging, install);

    InstallPlan {
        names: differing(&versions),
    }
}

/// Replace the installed files selected by `plan` with the staged copies.
fn apply(staging: &Path, install: &Path, plan: &InstallPlan) -> Result<(), InstallError> {
    let names: Vec<&str> = plan.names.iter().map(String::as_str).collect();

    replace_files(staging, install, &names).map_err(|error| {
        warn!("update: {error}");

        match error {
            ReplaceFilesError::Copy { .. } => InstallError::NotWritable,
            ReplaceFilesError::Replace { .. } => InstallError::Replace,
        }
    })?;

    // The file this process is running from is among the ones just renamed
    // aside, so at least one removal here is expected to fail. Startup sweeps
    // whatever survives.
    discard_previous(install);

    Ok(())
}

/// The version-resource key that decides whether the staged copy of `name` is
/// already the one on disk.
///
/// Comparing a version rather than the bytes is what keeps a rebuild of
/// unchanged sources from counting as a change, and the key differs for one
/// file: Explorer keeps a registered context-menu extension mapped in its own
/// process, so replacing that DLL costs a stale menu until Explorer restarts,
/// and its `InternalVersion` names the revision its own sources last changed in,
/// which is the only value that says whether the cost buys anything. Everything
/// else — the executables, the syntax-language DLL, and the vendored ConPTY
/// pair carrying Microsoft's version resource — moves with the `FileVersion` it
/// ships.
fn version_key(name: &str) -> &'static str {
    if name == SHELL_EXTENSION_DLL {
        "InternalVersion"
    } else {
        "FileVersion"
    }
}

/// The files `package` installs, which is everything the package holds.
///
/// The swap is performed by the instance an update replaces, so a name that
/// instance does not consider is a file that never gets installed. Reading the
/// list off the staged package rather than out of a list compiled into the
/// running build is therefore what lets a later release add a file at all: the
/// build performing the swap does not have to have heard of it.
///
/// The order is the sorted one so that a swap consumes the same list whatever
/// order the directory happens to enumerate in.
fn staged_names(package: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(package) else {
        warn!("update: {} cannot be listed", package.display());

        return Vec::new();
    };

    let mut names: Vec<String> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        // A name that is not Unicode is not one this project's packages carry,
        // and a lossy rendering of one would name a file that does not exist.
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();

    names.sort();

    names
}

/// The version each staged file carries in `staging` and in `install`.
fn versions(staging: &Path, install: &Path) -> Vec<(String, Option<String>, Option<String>)> {
    staged_names(staging)
        .into_iter()
        .map(|name| {
            let key = version_key(&name);
            let staged = version_string(&staging.join(&name), key);
            let installed = version_string(&install.join(&name), key);

            (name, staged, installed)
        })
        .collect()
}

/// The staged files whose copy differs from what is installed.
///
/// A version that cannot be read on either side counts as a difference. That
/// covers a release adding a file the installation does not have yet, and it
/// errs towards installing a file rather than towards leaving an installation
/// half-updated because one version resource could not be parsed.
fn differing(versions: &[(String, Option<String>, Option<String>)]) -> Vec<String> {
    versions
        .iter()
        .filter(|(_, staged, installed)| match (staged, installed) {
            (Some(staged), Some(installed)) => staged != installed,
            _ => true,
        })
        .map(|(name, _, _)| name.clone())
        .collect()
}

/// Start the installed executable and let it take over.
///
/// The single-instance mutex is released by process exit, so the successor is
/// told which process to wait for instead of being left to race it: reaching
/// the single-instance check too early makes it forward a request to the
/// instance on its way out and exit instead of starting.
fn relaunch(install: &Path, testing: bool) -> Result<(), InstallError> {
    let mut command = Command::new(install.join(APP_EXE));

    command.arg(AWAIT_EXIT_FLAG).arg(process::id().to_string());

    if testing {
        command.arg("--testing");
    }

    command.spawn().map(|_| ()).map_err(|error| {
        warn!("update: restarting failed: {error}");

        InstallError::Relaunch
    })
}
