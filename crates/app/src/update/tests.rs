use nmt_config::update::UpdateChannel;
use nmt_version::Version;

use crate::update::releases::{CheckError, Release, select, select_latest, supersedes};

fn version(label: &str) -> Version {
    Version::parse(label).expect("test labels are well formed")
}

/// A candidate the channel published, with no date. Only a comparison across
/// channels reads the date, so every same-channel case supplies none.
fn undated(label: &str) -> Release {
    Release {
        label: label.to_owned(),
        page_url: String::new(),
        assets: Vec::new(),
        published: None,
    }
}

fn published_on(label: &str, published: u32) -> Release {
    Release {
        published: Some(published),
        ..undated(label)
    }
}

#[test]
fn a_higher_release_supersedes_a_lower_one() {
    assert!(supersedes(&version("v1.2.0"), &undated("v1.3.0")));
    assert!(supersedes(&version("v1.2.0"), &undated("v1.2.1")));
    assert!(supersedes(&version("v1.9.0"), &undated("v2.0.0")));

    assert!(!supersedes(&version("v1.3.0"), &undated("v1.2.0")));
    assert!(!supersedes(&version("v1.2.0"), &undated("v1.2.0")));
    // Component order, which a lexical comparison of the labels would get
    // wrong once a number reaches two digits.
    assert!(!supersedes(&version("v1.10.0"), &undated("v1.9.0")));
    assert!(supersedes(&version("v1.9.0"), &undated("v1.10.0")));
}

#[test]
fn a_later_nightly_supersedes_an_earlier_one() {
    let installed = version("nightly-20260821-7567b41");

    assert!(supersedes(&installed, &undated("nightly-20260822-aaaaaaa")));
    assert!(!supersedes(
        &installed,
        &undated("nightly-20260820-aaaaaaa")
    ));
    assert!(!supersedes(
        &installed,
        &undated("nightly-20260821-7567b41")
    ));

    // Two revisions dated the same day: the published list is ordered by when
    // each release was cut, so a different one reached from that list is the
    // newer build rather than an ambiguous one.
    assert!(supersedes(&installed, &undated("nightly-20260821-aaaaaaa")));
}

#[test]
fn a_tag_that_cannot_be_read_is_still_offered() {
    assert!(supersedes(&version("v1.2.0"), &undated("build-4")));
}

#[test]
fn a_release_older_than_a_running_nightly_is_not_offered() {
    let installed = version("nightly-20260822-7567b41");

    // The case a locally built binary lands in: the release predates the
    // revision it was built from, so its lower number is not a downgrade to
    // offer.
    assert!(!supersedes(&installed, &published_on("v1.2.3", 20260814)));
    // A release cut the same day is the ambiguous case, and it is the one a
    // build made from that day's tree keeps being offered.
    assert!(!supersedes(&installed, &published_on("v1.2.3", 20260822)));
    // A later day carries revisions the nightly cannot have.
    assert!(supersedes(&installed, &published_on("v1.2.3", 20260823)));
    // With nothing to place it by, it cannot be shown to be ahead.
    assert!(!supersedes(&installed, &undated("v1.2.3")));
}

#[test]
fn a_nightly_is_offered_to_a_release_build() {
    // Moving to the nightly channel needs no publishing date: that channel is
    // only ever cut from the tip.
    assert!(supersedes(
        &version("v1.2.0"),
        &undated("nightly-20260821-7567b41")
    ));
}

/// Shaped like the releases page: newest first, with the entries this
/// repository actually carries from before the version naming settled.
const RELEASES: &str = r#"[
    { "tag_name": "nightly-20260822-bbbbbbb", "html_url": "https://example.invalid/n2",
      "draft": false, "prerelease": true },
    { "tag_name": "v1.4.0", "html_url": "https://example.invalid/draft",
      "draft": true, "prerelease": false },
    { "tag_name": "v1.3.0", "html_url": "https://example.invalid/r3",
      "draft": false, "prerelease": false },
    { "tag_name": "nightly-20260820-aaaaaaa", "html_url": "https://example.invalid/n1",
      "draft": false, "prerelease": true },
    { "tag_name": "build-4", "html_url": "https://example.invalid/b4",
      "draft": false, "prerelease": false },
    { "tag_name": "nightly-20260819", "html_url": "https://example.invalid/old",
      "draft": false, "prerelease": true },
    { "tag_name": "v1.2.0", "html_url": "https://example.invalid/r2",
      "draft": false, "prerelease": false }
]"#;

#[test]
fn each_channel_takes_its_own_newest_entry() {
    let stable = select(RELEASES, UpdateChannel::Stable).unwrap().unwrap();
    let nightly = select(RELEASES, UpdateChannel::Nightly).unwrap().unwrap();

    assert_eq!(stable.label, "v1.3.0");
    assert_eq!(stable.page_url, "https://example.invalid/r3");
    assert_eq!(nightly.label, "nightly-20260822-bbbbbbb");
}

#[test]
fn entries_that_cannot_be_compared_are_skipped() {
    // A draft is not published, `build-4` predates the naming, and
    // `nightly-20260819` predates the revision joining it. None of them can be
    // placed against a running build, so neither channel may offer one.
    let unusable = r#"[
        { "tag_name": "v1.4.0", "html_url": "https://example.invalid/draft",
          "draft": true, "prerelease": false },
        { "tag_name": "build-4", "html_url": "https://example.invalid/b4",
          "draft": false, "prerelease": false },
        { "tag_name": "nightly-20260819", "html_url": "https://example.invalid/old",
          "draft": false, "prerelease": true }
    ]"#;

    assert_eq!(select(unusable, UpdateChannel::Stable).unwrap(), None);
    assert_eq!(select(unusable, UpdateChannel::Nightly).unwrap(), None);
}

#[test]
fn a_prerelease_is_never_offered_as_stable() {
    let prerelease_tagged_as_release = r#"[
        { "tag_name": "v9.9.9", "html_url": "https://example.invalid/pre",
          "draft": false, "prerelease": true }
    ]"#;

    assert_eq!(
        select(prerelease_tagged_as_release, UpdateChannel::Stable).unwrap(),
        None
    );
}

#[test]
fn an_empty_channel_is_reported_as_nothing_found() {
    let only_stable = r#"[
        { "tag_name": "v1.2.0", "html_url": "https://example.invalid/r2",
          "draft": false, "prerelease": false }
    ]"#;

    assert_eq!(select(only_stable, UpdateChannel::Nightly).unwrap(), None);
}

#[test]
fn a_response_that_is_not_the_releases_list_is_rejected() {
    assert_eq!(
        select(
            r#"{"message":"API rate limit exceeded"}"#,
            UpdateChannel::Stable
        ),
        Err(CheckError::Unreadable)
    );
}

#[test]
fn the_latest_endpoint_answers_the_stable_channel() {
    let published = r#"{ "tag_name": "v1.3.0", "html_url": "https://example.invalid/r3",
        "draft": false, "prerelease": false }"#;

    let release = select_latest(published).unwrap().unwrap();
    assert_eq!(release.label, "v1.3.0");
    assert_eq!(release.page_url, "https://example.invalid/r3");

    // The endpoint promises the newest published non-prerelease, not that its
    // tag is one this build can be placed against.
    let predates_the_naming = r#"{ "tag_name": "build-4", "html_url": "https://example.invalid/b4",
        "draft": false, "prerelease": false }"#;
    assert_eq!(select_latest(predates_the_naming).unwrap(), None);

    // GitHub answers 404 for a repository with no full release yet, which
    // reaches this as a body that is not a release.
    assert_eq!(
        select_latest(r#"{"message":"Not Found"}"#),
        Err(CheckError::Unreadable)
    );
}

#[test]
fn a_publishing_timestamp_is_reduced_to_a_comparable_date() {
    let dated = r#"{ "tag_name": "v1.3.0", "html_url": "https://example.invalid/r3",
        "draft": false, "prerelease": false, "published_at": "2026-08-14T09:12:33Z" }"#;
    assert_eq!(
        select_latest(dated).unwrap().unwrap().published,
        Some(20260814)
    );

    // A timestamp in a shape this cannot read leaves the release unplaceable,
    // rather than dated from whatever sat at those offsets.
    let malformed = r#"{ "tag_name": "v1.3.0", "html_url": "https://example.invalid/r3",
        "draft": false, "prerelease": false, "published_at": "20260814T09:12:33Z" }"#;
    assert_eq!(select_latest(malformed).unwrap().unwrap().published, None);

    // A release that was never published carries a null timestamp, and the
    // recorded responses carry none at all.
    let null = r#"{ "tag_name": "v1.3.0", "html_url": "https://example.invalid/r3",
        "draft": false, "prerelease": false, "published_at": null }"#;
    assert_eq!(select_latest(null).unwrap().unwrap().published, None);
}
