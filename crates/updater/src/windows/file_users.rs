#[cfg(test)]
#[path = "file_users_tests.rs"]
mod tests;

use std::path::{Path, PathBuf};

use nmt_platform::windows::restart_manager::{
    AffectedApplication, FileUsage, RestartManagerError, RestartManagerSession, SystemApi,
};
use tracing::warn;

use crate::windows::InstallError;
use crate::windows::install::Installation;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileUsePromptReason {
    InUse,
    CheckFailed,
    RebootRequired,
    RemainingUsers,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileUsePrompt {
    pub reason: FileUsePromptReason,
    pub applications: Vec<AffectedApplication>,
}

trait FileUserSession: Send {
    fn open(path: &Path) -> Result<Self, RestartManagerError>
    where
        Self: Sized;

    fn file_usage(&self) -> Result<FileUsage, RestartManagerError>;

    fn shutdown(&self) -> Result<(), RestartManagerError>;

    fn restart(&self) -> Result<(), RestartManagerError>;
}

impl FileUserSession for RestartManagerSession {
    fn open(path: &Path) -> Result<Self, RestartManagerError> {
        RestartManagerSession::for_files(SystemApi, &[path])
    }

    fn file_usage(&self) -> Result<FileUsage, RestartManagerError> {
        RestartManagerSession::file_usage(self)
    }

    fn shutdown(&self) -> Result<(), RestartManagerError> {
        RestartManagerSession::shutdown(self)
    }

    fn restart(&self) -> Result<(), RestartManagerError> {
        RestartManagerSession::restart(self)
    }
}

enum ClosePreparation {
    Clear,
    Released {
        session: Box<dyn FileUserSession>,
        applications: Vec<AffectedApplication>,
    },
    Prompt(FileUsePrompt),
}

/// A captured shell-extension path that can be inspected or released on a
/// worker thread without borrowing the updater.
pub struct FileUsers {
    path: PathBuf,
}

impl FileUsers {
    pub(super) fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn inspect(self) -> Inspection {
        let usage = RestartManagerSession::for_files(SystemApi, &[&self.path])
            .and_then(|session| session.file_usage());

        Inspection(classify_file_usage(usage))
    }

    /// Close the applications identified by a fresh Restart Manager query.
    /// Call only after the user has accepted closing those applications.
    pub fn close(self) -> ClosedFileUsers {
        ClosedFileUsers(prepare_close_with::<RestartManagerSession>(&self.path))
    }
}

/// A completed inspection, consumed by the updater before any files change.
pub struct Inspection(pub(super) Result<Option<FileUsePrompt>, RestartManagerError>);

/// Keeps the Restart Manager session alive between a worker's shutdown and
/// the synchronous replacement/recovery step on the host thread.
pub struct ClosedFileUsers(ClosePreparation);

impl ClosedFileUsers {
    pub(super) fn apply(self, installation: &Installation) -> ApplyOutcome {
        match self.0 {
            ClosePreparation::Clear => match installation.apply() {
                Ok(()) => ApplyOutcome::Applied(Vec::new()),
                Err(error) => ApplyOutcome::Failed(error),
            },
            ClosePreparation::Prompt(prompt) => ApplyOutcome::Prompt(prompt),
            ClosePreparation::Released {
                session,
                applications,
            } => {
                // Recovery must run even when replacement fails: shutdown may
                // already have stopped applications the user was working in.
                let applied = installation.apply();
                let restarted = session.restart();

                if let Err(error) = &restarted {
                    warn!("update: restarting shell-extension users failed: {error}");
                }

                match applied {
                    Ok(()) => {
                        ApplyOutcome::Applied(recovery_application_names(&restarted, &applications))
                    }
                    Err(error) => ApplyOutcome::Failed(error),
                }
            }
        }
    }
}

pub(super) enum ApplyOutcome {
    Applied(Vec<String>),
    Prompt(FileUsePrompt),
    Failed(InstallError),
}

fn classify_file_usage(
    result: Result<FileUsage, RestartManagerError>,
) -> Result<Option<FileUsePrompt>, RestartManagerError> {
    let usage = result?;

    if usage.applications.is_empty() && usage.reboot_reasons.is_empty() {
        return Ok(None);
    }

    Ok(Some(FileUsePrompt {
        reason: if usage.reboot_reasons.is_empty() {
            FileUsePromptReason::InUse
        } else {
            FileUsePromptReason::RebootRequired
        },
        applications: usage.applications,
    }))
}

fn prepare_close_with<S>(path: &Path) -> ClosePreparation
where
    S: FileUserSession + 'static,
{
    let session = match S::open(path) {
        Ok(session) => session,
        Err(error) => {
            warn!("update: starting shell-extension shutdown failed: {error}");

            return check_failed_prompt();
        }
    };

    let usage = match session.file_usage() {
        Ok(usage) => usage,
        Err(error) => {
            warn!("update: refreshing shell-extension users failed: {error}");

            return check_failed_prompt();
        }
    };

    if !usage.reboot_reasons.is_empty() {
        return ClosePreparation::Prompt(FileUsePrompt {
            reason: FileUsePromptReason::RebootRequired,
            applications: usage.applications,
        });
    }

    if usage.applications.is_empty() {
        return ClosePreparation::Clear;
    }

    let applications = usage.applications;

    if let Err(error) = session.shutdown() {
        warn!("update: closing shell-extension users failed: {error}");

        if let Err(restart_error) = session.restart() {
            warn!("update: restoring partially closed applications failed: {restart_error}");
        }

        return match session.file_usage() {
            Ok(usage) => ClosePreparation::Prompt(FileUsePrompt {
                reason: if usage.reboot_reasons.is_empty() {
                    FileUsePromptReason::RemainingUsers
                } else {
                    FileUsePromptReason::RebootRequired
                },
                applications: usage.applications,
            }),
            Err(list_error) => {
                warn!("update: listing applications after failed shutdown failed: {list_error}");

                check_failed_prompt()
            }
        };
    }

    ClosePreparation::Released {
        session: Box::new(session),
        applications,
    }
}

fn check_failed_prompt() -> ClosePreparation {
    ClosePreparation::Prompt(FileUsePrompt {
        reason: FileUsePromptReason::CheckFailed,
        applications: Vec::new(),
    })
}

fn recovery_application_names(
    restarted: &Result<(), RestartManagerError>,
    applications: &[AffectedApplication],
) -> Vec<String> {
    applications
        .iter()
        .filter(|application| restarted.is_err() || !application.restartable)
        .cloned()
        .map(application_name)
        .collect()
}

fn application_name(application: AffectedApplication) -> String {
    if application.name.is_empty() {
        format!("PID {}", application.process_id)
    } else {
        application.name
    }
}
