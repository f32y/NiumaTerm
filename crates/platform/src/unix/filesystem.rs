use std::path::{Component, Path};
use std::{fs, io};

/// Move `source` over `destination`, replacing it if it exists.
///
/// POSIX `rename` already replaces an existing destination atomically within
/// one filesystem, so no separate replace call is needed here.
pub fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(source, destination)
}

/// Comparable lexical components without consulting the filesystem.
pub fn path_identity(path: &Path) -> Vec<String> {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect()
}

/// A stable component spelling used by persisted directory keys.
/// Root components and ASCII-only folding retain existing stored keys;
/// this representation is intentionally distinct from component comparison.
pub fn lexical_path_spelling(path: &Path) -> String {
    let mut normalized = String::new();
    for component in path.components() {
        if component == Component::CurDir {
            continue;
        }
        if !normalized.is_empty() {
            normalized.push('/');
        }
        normalized.push_str(&component.as_os_str().to_string_lossy());
    }

    normalized
}

/// Preserve full path spelling for installation keys, including separators.
/// Windows keys keep their existing ASCII folding; Unix names remain distinct.
pub fn installation_path_spelling(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}
