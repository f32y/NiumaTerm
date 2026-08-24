use crate::update::install::differing;

fn versions(
    entries: [(&'static str, Option<&str>, Option<&str>); 3],
) -> Vec<(&'static str, Option<String>, Option<String>)> {
    entries
        .into_iter()
        .map(|(name, staged, installed)| {
            (
                name,
                staged.map(str::to_owned),
                installed.map(str::to_owned),
            )
        })
        .collect()
}

#[test]
fn only_files_whose_version_moved_are_replaced() {
    let versions = versions([
        ("NiumaTerm.exe", Some("v1.3.0"), Some("v1.2.0")),
        // The release advanced, but this file's own revision did not, which is
        // the whole point of giving it a different key to be compared by.
        ("NmtShellExtension.dll", Some("a0e2c9f"), Some("a0e2c9f")),
        ("conpty.dll", Some("1.24.0"), Some("1.24.0")),
    ]);

    assert_eq!(differing(&versions), ["NiumaTerm.exe"]);
}

#[test]
fn a_version_that_cannot_be_read_counts_as_a_difference() {
    // In order: a file the installation does not have yet, a file that carries
    // no version resource on either side, and one whose staged copy could not
    // be read. None of them may be assumed to be current.
    let versions = versions([
        ("NiumaTerm.exe", Some("v1.3.0"), None),
        ("NmtAgentHook.exe", None, None),
        ("conpty.dll", None, Some("1.24.0")),
    ]);

    assert_eq!(
        differing(&versions),
        ["NiumaTerm.exe", "NmtAgentHook.exe", "conpty.dll"]
    );
}
