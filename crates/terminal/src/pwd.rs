use std::path;

pub(crate) fn pwd_to_path(pwd: &str) -> path::PathBuf {
    if let Some(rest) = pwd.strip_prefix("file://") {
        // rest = "host/path"; the path starts at the first '/'.
        if let Some(slash) = rest.find('/') {
            let path = &rest[slash..];

            // A drive-qualified Windows path is absolute without the URI slash.
            let path = if path.as_bytes().get(2) == Some(&b':') {
                &path[1..]
            } else {
                path
            };

            return path::PathBuf::from(path);
        }
    }

    path::PathBuf::from(pwd)
}
