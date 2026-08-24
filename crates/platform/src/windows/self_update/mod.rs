use std::error::Error;
use std::path::Path;
use std::{fmt, fs, io};

const PREVIOUS_SUFFIX: &str = ".nmt-previous";
const INCOMING_SUFFIX: &str = ".nmt-incoming";

#[derive(Debug)]
pub enum ReplaceFilesError {
    Copy { name: String, source: io::Error },
    Replace { name: String, source: io::Error },
}

impl fmt::Display for ReplaceFilesError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Copy { name, source } => write!(formatter, "copying {name} failed: {source}"),
            Self::Replace { name, source } => {
                write!(formatter, "replacing {name} failed: {source}")
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
            Err(source) => {
                undo(install, &done);
                discard_incoming(install, names);
                return Err(ReplaceFilesError::Replace {
                    name: (*name).to_string(),
                    source,
                });
            }
        }
    }
    Ok(())
}

fn replace(install: &Path, name: &str) -> io::Result<bool> {
    let target = install.join(name);
    let previous = install.join(format!("{name}{PREVIOUS_SUFFIX}"));
    let incoming = install.join(format!("{name}{INCOMING_SUFFIX}"));
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

pub fn discard_previous(install: &Path) {
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

#[cfg(test)]
mod tests;
