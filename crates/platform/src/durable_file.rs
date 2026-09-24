#[cfg(test)]
#[path = "durable_file_tests.rs"]
mod tests;

use std::io::{self, Write as _};
use std::path::Path;

use tempfile::NamedTempFile;

use crate::filesystem::replace_file_durable;

/// Replace `path` with `bytes` in one step.
///
/// The bytes go to a synced temporary file beside the target first, so
/// neither a crash mid-write nor a power loss after the replacement can leave
/// the target truncated, and a reader such as a starting shell never sees a
/// half-written file. Each call gets its own temporary name, so concurrent
/// writers cannot move each other's file away, and the temporary file is
/// removed when the replacement fails.
pub fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let directory = path
        .parent()
        .filter(|directory| !directory.as_os_str().is_empty())
        .unwrap_or(Path::new("."));

    let mut temporary = NamedTempFile::new_in(directory)?;

    temporary.write_all(bytes)?;

    temporary.as_file().sync_all()?;

    // Windows refuses to move a file that is still open, so the handle
    // closes first; the path it leaves behind still owns the cleanup.
    replace_file_durable(&temporary.into_temp_path(), path)
}
