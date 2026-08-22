use std::env;
use std::path::PathBuf;

use crate::windows::file_version::version_string;

/// A system DLL rather than one of this workspace's outputs: the reader has to
/// work before anything can be concluded from what our own build stamps, and
/// this file is present on every machine the tests run on.
fn system_dll() -> PathBuf {
    PathBuf::from(env::var_os("SystemRoot").expect("SystemRoot is set on Windows"))
        .join("System32")
        .join("kernel32.dll")
}

#[test]
fn reads_a_named_string_from_a_version_resource() {
    let path = system_dll();

    assert_eq!(
        version_string(&path, "CompanyName").as_deref(),
        Some("Microsoft Corporation")
    );
    // Microsoft's own resources are not written by the resource compiler this
    // workspace uses, so a readable value here is also evidence that the
    // translation table is being consulted rather than a default assumed.
    assert!(
        version_string(&path, "FileVersion")
            .is_some_and(|version| version.chars().next().is_some_and(|c| c.is_ascii_digit()))
    );
}

#[test]
fn an_absent_key_or_resource_reads_as_nothing() {
    assert_eq!(version_string(&system_dll(), "NoSuchKey"), None);
    // A path that resolves to no file at all, and one that resolves to a file
    // with no version resource: neither may be reported as a version.
    assert_eq!(
        version_string(
            &system_dll().with_file_name("no-such-file.dll"),
            "FileVersion"
        ),
        None
    );
    assert_eq!(version_string(&env::temp_dir(), "FileVersion"), None);
}
