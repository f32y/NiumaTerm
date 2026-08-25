//! Replacing the installed files with a staged release, in place.
//!
//! Windows refuses to delete a file that is mapped as a running image but
//! allows renaming one, which is what makes an installation replace itself
//! without a second program to do it: every file moves aside under a new name
//! and the staged copy takes the name it vacated. A process that has one of the
//! old files mapped — this one, and Explorer for the context-menu extension —
//! keeps running from the renamed file until it exits.

use std::path::Path;
use std::process;
use std::process::Command;
use std::time::Duration;

use nmt_platform::file_version::version_string;
use nmt_platform::windows::self_update::{
    ReplaceFilesError, discard_previous as discard_previous_files, replace_files,
};
use tracing::warn;

use crate::update::{AWAIT_EXIT_FLAG, InstallError};

const APP_EXE: &str = "NiumaTerm.exe";
pub(crate) const SHELL_EXTENSION_DLL: &str = "NmtShellExtension.dll";

/// Every file a package installs, and the version-resource key that decides
/// whether the staged copy is already the one on disk.
///
/// Comparing a version rather than the bytes is what keeps a rebuild of
/// unchanged sources from counting as a change, and it is per file because the
/// files do not move together:
///
/// - The two executables and their release label advance with every release.
/// - Explorer keeps a registered context-menu extension mapped in its own
///   process, so replacing that DLL costs a stale menu until Explorer restarts.
///   Its `InternalVersion` names the revision its own sources last changed in,
///   which is the only value that says whether the cost buys anything.
/// - The ConPTY pair is a vendored Microsoft build carrying Microsoft's version
///   resource, which moves only when the vendored copy is replaced.
const PAYLOAD: [(&str, &str); 5] = [
    (APP_EXE, "FileVersion"),
    ("NmtAgentHook.exe", "FileVersion"),
    (SHELL_EXTENSION_DLL, "InternalVersion"),
    ("conpty.dll", "FileVersion"),
    ("OpenConsole.exe", "FileVersion"),
];

/// How long a restarting instance waits for its predecessor before starting
/// anyway. Waiting at all is what keeps it from reaching the single-instance
/// check while the old process still holds the mutex; waiting forever would
/// hand a shutdown that never finishes the power to prevent the restart.
pub(crate) const PREDECESSOR_TIMEOUT: Duration = Duration::from_secs(30);

/// The version each payload file carries in `staging` and in `install`.
///
/// A payload name the package does not carry is left out entirely rather than
/// counted as a difference: a release that stops shipping a file installs
/// nothing for it, and asking to copy a file that was never unpacked would fail
/// the whole update over a file the new build does not need.
fn versions(staging: &Path, install: &Path) -> Vec<(&'static str, Option<String>, Option<String>)> {
    PAYLOAD
        .iter()
        .filter(|(name, _)| staging.join(name).is_file())
        .map(|(name, key)| {
            (
                *name,
                version_string(&staging.join(name), key),
                version_string(&install.join(name), key),
            )
        })
        .collect()
}

/// The payload files whose staged copy differs from what is installed.
///
/// A version that cannot be read on either side counts as a difference. That
/// covers a release adding a file the installation does not have yet, and it
/// errs towards installing a file rather than towards leaving an installation
/// half-updated because one version resource could not be parsed.
fn differing(versions: &[(&'static str, Option<String>, Option<String>)]) -> Vec<&'static str> {
    versions
        .iter()
        .filter(|(_, staged, installed)| match (staged, installed) {
            (Some(staged), Some(installed)) => staged != installed,
            _ => true,
        })
        .map(|(name, _, _)| *name)
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InstallPlan {
    names: Vec<&'static str>,
}

impl InstallPlan {
    pub(crate) fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    pub(crate) fn contains(&self, name: &str) -> bool {
        self.names.contains(&name)
    }
}

/// Select the staged payload files that differ from the installed copies.
///
/// Capturing this before any file moves keeps later pre-install decisions tied
/// to the exact set that the swap will consume.
pub(crate) fn plan(staging: &Path, install: &Path) -> InstallPlan {
    let versions = versions(staging, install);
    InstallPlan {
        names: differing(&versions),
    }
}

/// Replace the installed payload selected by `plan` with the staged copies.
pub(crate) fn apply(
    staging: &Path,
    install: &Path,
    plan: &InstallPlan,
) -> Result<(), InstallError> {
    replace_files(staging, install, &plan.names).map_err(|error| {
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

/// Remove the files a previous update renamed aside. The ones still mapped by a
/// process that outlived the update — Explorer, most often — refuse to go and
/// are left for a later run to collect.
pub(crate) fn discard_previous(install: &Path) {
    discard_previous_files(install);
}

/// Start the installed executable and let it take over.
///
/// The single-instance mutex is released by process exit, so the successor is
/// told which process to wait for instead of being left to race it: reaching
/// the single-instance check too early makes it forward a request to the
/// instance on its way out and exit instead of starting.
pub(crate) fn relaunch(install: &Path, testing: bool) -> Result<(), InstallError> {
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

#[cfg(test)]
mod tests;
