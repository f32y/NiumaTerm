#[cfg(test)]
#[path = "self_update_tests.rs"]
mod self_update_tests;

use std::error::Error;
use std::path::Path;
use std::{fmt, fs, io};

const PREVIOUS_SUFFIX: &str = ".nmt-previous";
const INCOMING_SUFFIX: &str = ".nmt-incoming";

#[derive(Debug)]
pub enum ReplaceFilesError {
    Copy {
        name: String,
        source: io::Error,
    },
    Replace {
        name: String,
        source: io::Error,
        rollback_errors: Vec<String>,
    },
}

impl fmt::Display for ReplaceFilesError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Copy { name, source } => write!(formatter, "copying {name} failed: {source}"),
            Self::Replace {
                name,
                source,
                rollback_errors,
            } => {
                write!(formatter, "replacing {name} failed: {source}")?;

                for error in rollback_errors {
                    write!(formatter, "; restoration failed: {error}")?;
                }

                Ok(())
            }
        }
    }
}

impl Error for ReplaceFilesError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Copy { source, .. } | Self::Replace { source, .. } => Some(source),
        }
    }
}

pub fn replace_files(
    staging: &Path,
    install: &Path,
    names: &[&str],
) -> Result<(), ReplaceFilesError> {
    copy_in(staging, install, names)?;

    swap(install, names)
}

fn copy_in(staging: &Path, install: &Path, names: &[&str]) -> Result<(), ReplaceFilesError> {
    for name in names {
        let incoming = install.join(format!("{name}{INCOMING_SUFFIX}"));

        if let Err(source) = fs::copy(staging.join(name), &incoming) {
            discard_incoming(install, names);

            return Err(ReplaceFilesError::Copy {
                name: (*name).to_string(),
                source,
            });
        }
    }

    Ok(())
}

fn swap(install: &Path, names: &[&str]) -> Result<(), ReplaceFilesError> {
    let mut done: Vec<(&str, bool)> = Vec::new();

    for name in names {
        match replace(install, name) {
            Ok(had_previous) => done.push((name, had_previous)),
            Err(mut failure) => {
                failure.rollback_errors.extend(undo(install, &done));

                if failure.rollback_errors.is_empty() {
                    discard_incoming(install, names);
                }

                return Err(ReplaceFilesError::Replace {
                    name: (*name).to_string(),
                    source: failure.source,
                    rollback_errors: failure.rollback_errors,
                });
            }
        }
    }

    Ok(())
}

struct SwapFailure {
    source: io::Error,
    rollback_errors: Vec<String>,
}

fn replace(install: &Path, name: &str) -> Result<bool, SwapFailure> {
    let target = install.join(name);
    let previous = install.join(format!("{name}{PREVIOUS_SUFFIX}"));
    let incoming = install.join(format!("{name}{INCOMING_SUFFIX}"));
    let had_previous = target.exists();

    if had_previous {
        fs::rename(&target, &previous).map_err(|source| SwapFailure {
            source,
            rollback_errors: Vec::new(),
        })?;
    }

    match fs::rename(&incoming, &target) {
        Ok(()) => Ok(had_previous),
        Err(source) => {
            let mut rollback_errors = Vec::new();

            if had_previous && let Err(error) = fs::rename(&previous, &target) {
                rollback_errors.push(format!(
                    "{} to {}: {error}",
                    previous.display(),
                    target.display()
                ));
            }

            Err(SwapFailure {
                source,
                rollback_errors,
            })
        }
    }
}

fn undo(install: &Path, done: &[(&str, bool)]) -> Vec<String> {
    let mut errors = Vec::new();

    for (name, had_previous) in done.iter().rev() {
        let target = install.join(name);
        let incoming = install.join(format!("{name}{INCOMING_SUFFIX}"));

        if let Err(error) = fs::rename(&target, &incoming) {
            errors.push(format!(
                "{} to {}: {error}",
                target.display(),
                incoming.display()
            ));

            continue;
        }

        if *had_previous {
            let previous = install.join(format!("{name}{PREVIOUS_SUFFIX}"));

            if let Err(error) = fs::rename(&previous, &target) {
                errors.push(format!(
                    "{} to {}: {error}",
                    previous.display(),
                    target.display()
                ));
            }
        }
    }

    errors
}

fn discard_incoming(install: &Path, names: &[&str]) {
    for name in names {
        let _ = fs::remove_file(install.join(format!("{name}{INCOMING_SUFFIX}")));
    }
}

pub fn discard_previous(install: &Path) {
    let Ok(entries) =
        fs::read_dir(install).and_then(|entries| entries.collect::<io::Result<Vec<_>>>())
    else {
        return;
    };

    // Incoming files remain when restoration did not finish. Preserve every
    // backup until the installation has a complete set of replacement files.
    if entries.iter().any(|entry| {
        entry
            .file_name()
            .to_string_lossy()
            .ends_with(INCOMING_SUFFIX)
    }) {
        return;
    }

    for entry in entries {
        let name = entry.file_name();
        let name = name.to_string_lossy();

        if let Some(original) = name.strip_suffix(PREVIOUS_SUFFIX)
            && install.join(original).is_file()
        {
            let _ = fs::remove_file(entry.path());
        }
    }
}
