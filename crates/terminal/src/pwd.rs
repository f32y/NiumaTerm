use std::path;

pub(crate) fn pwd_to_path(pwd: &str) -> path::PathBuf {
    if let Some(rest) = pwd.strip_prefix("file://") {
        // rest = "host/path"; the path starts at the first '/'.
        if let Some(slash) = rest.find('/') {
            return path::PathBuf::from(&rest[slash..]);
        }
    }

    path::PathBuf::from(pwd)
}
