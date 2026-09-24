use std::fs;

use tempfile::tempdir;

use crate::durable_file::write;

#[test]
fn replaces_the_target_and_leaves_no_temporary_file() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("settings.json");

    write(&path, b"first").unwrap();
    write(&path, b"second").unwrap();

    assert_eq!(fs::read(&path).unwrap(), b"second");
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
}

#[test]
fn a_failed_replacement_removes_the_temporary_file() {
    let directory = tempdir().unwrap();
    let target = directory.path().join("occupied");

    // A directory cannot be replaced by a file, so the final step fails.
    fs::create_dir(&target).unwrap();

    assert!(write(&target, b"data").is_err());
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
}
