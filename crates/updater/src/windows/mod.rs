//! Windows update state and installation policy. Blocking jobs carry their own
//! inputs; the host schedules them and handles the returned user decisions.

pub use crate::windows::download::Download;
pub use crate::windows::file_users::{
    ClosedFileUsers, FileUsePrompt, FileUsePromptReason, FileUsers, Inspection,
};
pub use crate::windows::install::{InstallError, Installation};
pub use crate::windows::releases::{Check, CheckError, CheckedRelease, Release};
pub use crate::windows::status::Status;

pub mod restart_manager;

mod download;
mod file_users;
mod file_version;
mod install;
mod process_exit;
mod releases;
mod self_update;
mod status;

#[cfg(test)]
mod tests;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use nmt_config::update::UpdateConfig;
use tracing::warn;

use crate::windows::file_users::ApplyOutcome;
use crate::windows::self_update::discard_previous;

const STAGING_DIRECTORY: &str = "update";

/// Delay the first network request until startup has finished opening windows.
pub const FIRST_CHECK_DELAY: Duration = Duration::from_secs(5);

pub const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

/// What the host must do next after an installation step.
#[derive(Debug, PartialEq, Eq)]
pub enum InstallAction {
    None,
    InspectFileUsers,
    Prompt(FileUsePrompt),
    RecoveryWarning(Vec<String>),
    Relaunch,
}

pub struct Updater {
    status: Status,
    settings: UpdateConfig,
    version: &'static str,
    install: PathBuf,
    staging: PathBuf,
    testing: bool,
    pending: Option<Installation>,
}

impl Updater {
    pub fn new(
        version: &'static str,
        install: PathBuf,
        config_directory: &Path,
        settings: UpdateConfig,
        testing: bool,
    ) -> Self {
        Self {
            status: Status::Unknown,
            settings,
            version,
            install,
            staging: config_directory.join(STAGING_DIRECTORY),
            testing,
            pending: None,
        }
    }

    pub fn status(&self) -> &Status {
        &self.status
    }

    pub fn testing(&self) -> bool {
        self.testing
    }

    pub fn automatic_checks_enabled(&self) -> bool {
        !self.testing && self.settings.check_updates
    }

    /// Update the remembered settings and report whether automatic checking
    /// needs a new request. An installation keeps the release it started with.
    pub fn set_settings(&mut self, settings: UpdateConfig) -> bool {
        let switched = self.settings.channel != settings.channel;
        let turned_on = settings.check_updates && !self.settings.check_updates;

        self.settings = settings;

        if self.pending.is_some() || self.status.installation_in_progress() {
            return false;
        }

        if switched {
            self.status = Status::Unknown;
        }

        self.automatic_checks_enabled() && (switched || turned_on)
    }

    pub fn begin_check(&mut self) -> Option<Check> {
        if self.status.busy() || self.pending.is_some() {
            return None;
        }

        self.status = Status::Checking;

        Some(Check {
            channel: self.settings.channel,
            version: self.version,
        })
    }

    /// Accept a completed check only while its channel is still selected.
    pub fn finish_check(&mut self, checked: CheckedRelease) -> bool {
        if checked.channel != self.settings.channel {
            return false;
        }

        self.status = checked.status;

        true
    }

    pub fn begin_install(&mut self) -> Option<Download> {
        let Status::Available(release) = &self.status else {
            return None;
        };

        let download = Download {
            release: release.clone(),
            staging: self.staging.clone(),
            install: self.install.clone(),
            version: self.version,
            testing: self.testing,
        };

        self.status = Status::Installing(release.clone());

        Some(download)
    }

    pub fn finish_download(
        &mut self,
        downloaded: Result<Installation, InstallError>,
    ) -> InstallAction {
        let installation = match downloaded {
            Ok(installation) => installation,
            Err(error) => return self.fail_install(error),
        };

        let inspect = installation.changes_shell_extension();

        self.pending = Some(installation);

        if inspect {
            InstallAction::InspectFileUsers
        } else {
            self.continue_install()
        }
    }

    pub fn inspect_file_users(&mut self) -> Option<FileUsers> {
        let pending = self.pending.as_ref()?;

        self.status = Status::InspectingFileUse(pending.release().clone());

        Some(FileUsers::new(pending.shell_extension()))
    }

    pub fn finish_inspection(&mut self, inspection: Inspection) -> InstallAction {
        let prompt = match inspection.0 {
            Ok(None) => return self.continue_install(),
            Ok(Some(prompt)) => prompt,
            Err(error) => {
                warn!("update: checking shell-extension users failed: {error}");

                FileUsePrompt {
                    reason: FileUsePromptReason::CheckFailed,
                    applications: Vec::new(),
                }
            }
        };

        self.await_file_users(prompt)
    }

    pub fn close_file_users(&mut self) -> Option<FileUsers> {
        let pending = self.pending.as_ref()?;

        self.status = Status::ClosingFileUsers(pending.release().clone());

        Some(FileUsers::new(pending.shell_extension()))
    }

    /// Replace files and recover closed applications in one synchronous step.
    /// A host must handle `Relaunch` before dispatching unrelated callbacks,
    /// because the running executable may now have a temporary filename.
    pub fn finish_closing(&mut self, closed: ClosedFileUsers) -> InstallAction {
        let Some(pending) = self.pending.as_ref() else {
            return InstallAction::None;
        };

        self.status = Status::Installing(pending.release().clone());

        match closed.apply(pending) {
            ApplyOutcome::Applied(applications) if applications.is_empty() => {
                InstallAction::Relaunch
            }
            ApplyOutcome::Applied(applications) => {
                self.status = Status::RecoveryWarning {
                    release: pending.release().clone(),
                    applications: applications.clone(),
                };

                InstallAction::RecoveryWarning(applications)
            }
            ApplyOutcome::Prompt(prompt) => self.await_file_users(prompt),
            ApplyOutcome::Failed(error) => self.fail_install(error),
        }
    }

    pub fn continue_install(&mut self) -> InstallAction {
        let Some(pending) = self.pending.as_ref() else {
            return InstallAction::None;
        };

        self.status = Status::Installing(pending.release().clone());

        match pending.apply() {
            Ok(()) => InstallAction::Relaunch,
            Err(error) => self.fail_install(error),
        }
    }

    /// Return whether the installed executable was started. On success the
    /// host must quit without rebuilding state from the old executable path.
    pub fn relaunch(&mut self) -> bool {
        let Some(pending) = self.pending.take() else {
            return false;
        };

        match pending.relaunch() {
            Ok(()) => true,
            Err(error) => {
                self.fail_install(error);

                false
            }
        }
    }

    pub fn cancel_install(&mut self) -> bool {
        let Some(pending) = self.pending.take() else {
            return false;
        };

        self.status = Status::Available(pending.release().clone());

        true
    }

    fn await_file_users(&mut self, prompt: FileUsePrompt) -> InstallAction {
        let Some(pending) = self.pending.as_ref() else {
            return InstallAction::None;
        };

        self.status = Status::AwaitingFileUse(pending.release().clone());

        InstallAction::Prompt(prompt)
    }

    fn fail_install(&mut self, error: InstallError) -> InstallAction {
        self.pending = None;
        self.status = Status::InstallFailed(error);

        InstallAction::None
    }
}

/// Restore package additions and discard files left by a previous update.
pub fn settle_previous_update(config_directory: &Path, install: &Path) {
    let staging = config_directory.join(STAGING_DIRECTORY);

    if let Ok(entries) = fs::read_dir(&staging) {
        for entry in entries.flatten() {
            install::install_additions(&entry.path(), install);
        }
    }

    // Images mapped by the previous process can be removed after it exits.
    // Explorer may retain the old extension until it next restarts.
    discard_previous(install);

    // A retry downloads again instead of trusting an abandoned staged package.
    let _ = fs::remove_dir_all(&staging);
}

pub fn wait_for_previous_instance(pid: u32) -> bool {
    process_exit::wait_for_exit(pid, install::PREDECESSOR_TIMEOUT)
}
