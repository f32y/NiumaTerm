use std::io;
use std::os::windows::ffi::OsStrExt as _;
use std::path::{Component, Path};

use windows_sys::Win32::Storage::FileSystem::{
    MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
};

pub fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();

    let replaced = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if replaced == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Comparable lexical components without consulting the filesystem.
pub fn path_identity(path: &Path) -> Vec<String> {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy().to_lowercase())
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
    normalized.make_ascii_lowercase();
    normalized
}

/// Preserve full path spelling for installation keys, including separators.
/// Windows keys keep their existing ASCII folding; Unix names remain distinct.
pub fn installation_path_spelling(path: &Path) -> String {
    path.to_string_lossy().to_ascii_lowercase()
}
