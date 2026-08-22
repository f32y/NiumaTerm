//! Replacing the installed files with a staged release, in place.
//!
//! Windows refuses to delete a file that is mapped as a running image but
//! allows renaming one, which is what makes an installation replace itself
//! without a second program to do it: every file moves aside under a new name
//! and the staged copy takes the name it vacated. A process that has one of the
//! old files mapped — this one, and Explorer for the context-menu extension —
//! keeps running from the renamed file until it exits.

use std::path::Path;
use std::process::Command;
use std::time::Duration;
use std::{fs, io, process};

use nmt_platform::file_version::version_string;
use tracing::warn;

use crate::update::{AWAIT_EXIT_FLAG, InstallError};

const APP_EXE: &str = "NiumaTerm.exe";

/// Names the swap gives to the file it is about to replace and to the copy that
/// will replace it. Both live in the installation directory, so the renames
/// that follow a copy stay within one directory and cannot fail for lack of
/// space or cross a volume boundary.
const PREVIOUS_SUFFIX: &str = ".nmt-previous";
const INCOMING_SUFFIX: &str = ".nmt-incoming";

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
    ("NiumaTermHook.exe", "FileVersion"),
    ("shell_extension.dll", "InternalVersion"),
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

/// Replace the installed payload with the one in `staging`, and report which
/// files were touched.
pub(crate) fn apply(staging: &Path, install: &Path) -> Result<Vec<&'static str>, InstallError> {
    let versions = versions(staging, install);
    let names = differing(&versions);

    copy_in(staging, install, &names)?;
    swap(install, &names)?;

    // The file this process is running from is among the ones just renamed
    // aside, so at least one removal here is expected to fail. Startup sweeps
    // whatever survives.
    discard_previous(install);

    Ok(names)
}

/// Copy each staged file next to the file it will replace.
///
/// This is the only step that can fail for an ordinary reason — no space, a
/// read-only installation directory, a scanner holding a handle open — so it
/// runs to completion before anything installed is touched, and undoes itself
/// on failure. It is also what proves the directory is writable, rather than
/// asking and acting on an answer that could change in between.
fn copy_in(staging: &Path, install: &Path, names: &[&str]) -> Result<(), InstallError> {
    for name in names {
        let incoming = install.join(format!("{name}{INCOMING_SUFFIX}"));

        if let Err(error) = fs::copy(staging.join(name), &incoming) {
            warn!("update: staging {name} failed: {error}");
            discard_incoming(install, names);

            return Err(InstallError::NotWritable);
        }
    }

    Ok(())
}

/// Give every copied file the name of the one it replaces.
///
/// Each step is a rename within one directory, so reaching this point means the
/// remaining work is metadata only. A failure anyway leaves the installation as
/// it was rather than partly updated.
fn swap(install: &Path, names: &[&str]) -> Result<(), InstallError> {
    let mut done: Vec<(&str, bool)> = Vec::new();

    for name in names {
        match replace(install, name) {
            Ok(had_previous) => done.push((name, had_previous)),
            Err(error) => {
                warn!("update: replacing {name} failed: {error}");
                undo(install, &done);
                discard_incoming(install, names);

                return Err(InstallError::Replace);
            }
        }
    }

    Ok(())
}

/// Reports whether an installed file was moved aside, which is what an undo has
/// to put back.
fn replace(install: &Path, name: &str) -> io::Result<bool> {
    let target = install.join(name);
    let previous = install.join(format!("{name}{PREVIOUS_SUFFIX}"));
    let incoming = install.join(format!("{name}{INCOMING_SUFFIX}"));

    // A release that adds a file reaches an installation with nothing to move
    // aside, and then claiming the free name is the whole operation.
    let had_previous = target.exists();

    if had_previous {
        fs::rename(&target, &previous)?;
    }

    match fs::rename(&incoming, &target) {
        Ok(()) => Ok(had_previous),
        Err(error) => {
            if had_previous {
                let _ = fs::rename(&previous, &target);
            }

            Err(error)
        }
    }
}

fn undo(install: &Path, done: &[(&str, bool)]) {
    for (name, had_previous) in done {
        let target = install.join(name);

        let _ = fs::rename(&target, install.join(format!("{name}{INCOMING_SUFFIX}")));

        if *had_previous {
            let _ = fs::rename(install.join(format!("{name}{PREVIOUS_SUFFIX}")), &target);
        }
    }
}

fn discard_incoming(install: &Path, names: &[&str]) {
    for name in names {
        let _ = fs::remove_file(install.join(format!("{name}{INCOMING_SUFFIX}")));
    }
}

/// Remove the files a previous update renamed aside. The ones still mapped by a
/// process that outlived the update — Explorer, most often — refuse to go and
/// are left for a later run to collect.
pub(crate) fn discard_previous(install: &Path) {
    let Ok(entries) = fs::read_dir(install) else {
        return;
    };

    for entry in entries.flatten() {
        if entry
            .file_name()
            .to_string_lossy()
            .ends_with(PREVIOUS_SUFFIX)
        {
            let _ = fs::remove_file(entry.path());
        }
    }
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
