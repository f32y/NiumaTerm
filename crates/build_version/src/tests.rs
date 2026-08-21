use crate::{is_nightly, is_release};

#[test]
fn accepts_the_two_published_forms() {
    assert!(is_release("v1.2.0"));
    assert!(is_release("v10.0.11"));
    assert!(is_nightly("nightly-20260821-7567b41"));
    // `rev-parse --short` widens the abbreviation when seven characters would
    // be ambiguous.
    assert!(is_nightly("nightly-20260821-7567b41a"));
}

#[test]
fn rejects_everything_else() {
    // The forms each other's prefix would otherwise admit.
    assert!(!is_release("1.2.0"));
    assert!(!is_nightly("20260821-7567b41"));

    // Truncated, padded, or non-numeric release components.
    assert!(!is_release("v1.2"));
    assert!(!is_release("v1.2.0.1"));
    assert!(!is_release("v1.2."));
    assert!(!is_release("v1.2.0-rc1"));

    // The date and commit a nightly is identified by.
    assert!(!is_nightly("nightly-20260821"));
    assert!(!is_nightly("nightly-2026821-7567b41"));
    assert!(!is_nightly("nightly-20260821-7567b4"));
    assert!(!is_nightly("nightly-20260821-branchxx"));

    // The forms this replaced: a bare commit and the crate version.
    assert!(!is_release("7567b41") && !is_nightly("7567b41"));
    assert!(!is_release("nightly-20260820") && !is_nightly("nightly-20260820"));
}
