use crate::Version;

#[test]
fn reads_the_two_published_forms() {
    assert_eq!(
        Version::parse("v1.2.0"),
        Some(Version::Release {
            major: 1,
            minor: 2,
            patch: 0
        })
    );
    assert_eq!(
        Version::parse("v10.0.11"),
        Some(Version::Release {
            major: 10,
            minor: 0,
            patch: 11
        })
    );
    assert_eq!(
        Version::parse("nightly-20260821-7567b41"),
        Some(Version::Nightly {
            date: 20260821,
            commit: "7567b41".to_owned()
        })
    );
    // `rev-parse --short` widens the abbreviation when seven characters would
    // be ambiguous.
    assert_eq!(
        Version::parse("nightly-20260821-7567b41a"),
        Some(Version::Nightly {
            date: 20260821,
            commit: "7567b41a".to_owned()
        })
    );
}

#[test]
fn rejects_everything_else() {
    for label in [
        // The forms each other's prefix would otherwise admit.
        "1.2.0",
        "20260821-7567b41",
        // Truncated, padded, or non-numeric release components.
        "v1.2",
        "v1.2.0.1",
        "v1.2.",
        "v1.2.0-rc1",
        "v1.+2.0",
        "v1. 2.0",
        // The date and commit a nightly is identified by.
        "nightly-20260821",
        "nightly-2026821-7567b41",
        "nightly-20260821-7567b4",
        "nightly-20260821-branchxx",
        // The forms this replaced: a bare commit, the crate version, and the
        // nightly tags published before the revision joined the name.
        "7567b41",
        "nightly-20260820",
        "build-4",
        "",
    ] {
        assert_eq!(Version::parse(label), None, "`{label}` should not parse");
    }
}

#[test]
fn channels_are_comparable_only_with_themselves() {
    let release = Version::parse("v1.2.0").unwrap();
    let newer_release = Version::parse("v1.3.0").unwrap();
    let nightly = Version::parse("nightly-20260821-7567b41").unwrap();

    assert!(release.same_channel(&newer_release));
    assert!(nightly.same_channel(&nightly.clone()));
    assert!(!release.same_channel(&nightly));
    assert!(!nightly.same_channel(&release));
}
