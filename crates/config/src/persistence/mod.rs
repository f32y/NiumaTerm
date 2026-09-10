use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::path::Path;

use nmt_platform::filesystem::replace_file;
use tempfile::NamedTempFile;

pub(crate) fn read(path: &Path) -> io::Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(content) => Ok(Some(content)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Serialize cooperating writers across the entire read-modify-replace cycle.
/// The lock lives beside the target: replacing the target changes its file
/// identity, so locking the target itself would let a later writer bypass it.
pub(crate) fn update(
    path: &Path,
    edit: impl FnOnce(Option<&str>) -> io::Result<String>,
) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));

    fs::create_dir_all(parent)?;

    let mut lock_name = path
        .file_name()
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "missing configuration filename",
            )
        })?
        .to_os_string();

    lock_name.push(".lock");

    // Keep this file after releasing the lock so waiting and newly arriving
    // processes always lock the same file identity.
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(parent.join(lock_name))?;

    lock.lock()?;

    let current = read(path)?;
    let content = edit(current.as_deref())?;
    let mut temporary = NamedTempFile::new_in(parent)?;

    temporary.write_all(content.as_bytes())?;
    temporary.as_file().sync_all()?;

    // Close the temporary handle before replacement; its path still owns
    // cleanup if replacement fails.
    let temporary = temporary.into_temp_path();

    replace_file(&temporary, path)
}

#[cfg(test)]
mod tests;
