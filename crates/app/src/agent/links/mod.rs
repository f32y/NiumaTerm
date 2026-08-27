use std::path::{Path, PathBuf};

use gpui::App;

pub(super) fn open(target: &str, cwd: Option<&Path>, cx: &mut App) {
    if let Some(path) = resolve_local_path(target, cwd).filter(|path| path.exists()) {
        cx.open_with_system(&path);
    } else {
        cx.open_url(target);
    }
}

fn resolve_local_path(target: &str, cwd: Option<&Path>) -> Option<PathBuf> {
    let target = strip_source_position(target.trim());
    if target.is_empty() || has_uri_scheme(target) {
        return None;
    }

    let target = strip_extra_drive_slash(target);
    let path = PathBuf::from(target);
    if path.is_absolute() || has_windows_root(target) {
        Some(path)
    } else {
        cwd.map(|cwd| cwd.join(path))
    }
}

fn strip_source_position(mut target: &str) -> &str {
    for _ in 0..2 {
        let Some((path, position)) = target.rsplit_once(':') else {
            break;
        };
        if position.is_empty() || !position.bytes().all(|byte| byte.is_ascii_digit()) {
            break;
        }
        target = path;
    }
    target
}

fn has_uri_scheme(target: &str) -> bool {
    let Some((scheme, _)) = target.split_once(':') else {
        return false;
    };
    if scheme.len() == 1 {
        return false;
    }

    let mut chars = scheme.chars();
    chars.next().is_some_and(|ch| ch.is_ascii_alphabetic())
        && chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '.'))
}

fn has_windows_root(target: &str) -> bool {
    let bytes = target.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\')
}

fn strip_extra_drive_slash(target: &str) -> &str {
    let bytes = target.as_bytes();
    if bytes.len() >= 4
        && bytes[0] == b'/'
        && bytes[1].is_ascii_alphabetic()
        && bytes[2] == b':'
        && matches!(bytes[3], b'/' | b'\\')
    {
        &target[1..]
    } else {
        target
    }
}

#[cfg(test)]
mod tests;
