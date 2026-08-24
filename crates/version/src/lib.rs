//! The version label every binary this workspace links is stamped with, and
//! that every published package and release tag carries.
//!
//! A label has exactly one of two forms, so that a running installation and a
//! release on GitHub can be compared without translating between them:
//!
//! - release: `v1.2.0`, the tag a release build was cut from
//! - nightly: `nightly-20260821-7567b41`, the committer date and revision a
//!   build came from
//!
//! [`Version::parse`] reads a label back into its parts. Whether one label
//! supersedes another is a question for whoever is deciding to update, not for
//! the format: the answer depends on which channel was chosen and on what a
//! same-day nightly rebuild should mean.
//!
//! [`emit`] produces a label at build time and rejects anything outside the two
//! forms, so a binary whose version cannot be compared with a release never
//! ships. [`crate_revision`] answers a different question, for a binary an
//! update leaves in place unless it changed.

mod build;

#[cfg(test)]
mod tests;

pub use crate::build::{crate_revision, emit, emit_internal};

/// One version label, parsed into the parts a comparison needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Version {
    /// `v1.2.0`, ordered by its three numbers.
    Release { major: u32, minor: u32, patch: u32 },
    /// `nightly-20260821-7567b41`. The date is the committer date as
    /// `yyyymmdd`, which orders numerically because it is zero-padded.
    Nightly { date: u32, commit: String },
}

impl Version {
    pub fn parse(label: &str) -> Option<Self> {
        parse_release(label).or_else(|| parse_nightly(label))
    }

    /// Whether two labels came from the same publishing channel, which is what
    /// makes their parts comparable at all.
    pub fn same_channel(&self, other: &Self) -> bool {
        matches!(
            (self, other),
            (Self::Release { .. }, Self::Release { .. })
                | (Self::Nightly { .. }, Self::Nightly { .. })
        )
    }
}

fn parse_release(label: &str) -> Option<Version> {
    let mut parts = label.strip_prefix('v')?.split('.');
    let major = number(parts.next()?)?;
    let minor = number(parts.next()?)?;
    let patch = number(parts.next()?)?;

    parts.next().is_none().then_some(Version::Release {
        major,
        minor,
        patch,
    })
}

fn parse_nightly(label: &str) -> Option<Version> {
    let (date, commit) = label.strip_prefix("nightly-")?.split_once('-')?;

    // An abbreviated commit is at least seven characters and grows only when
    // that many would be ambiguous, so the length is a lower bound rather than
    // an exact width.
    if commit.len() < 7 || !commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }

    Some(Version::Nightly {
        date: (date.len() == 8).then(|| number(date)).flatten()?,
        commit: commit.to_owned(),
    })
}

/// Rejects a sign, a leading plus, and surrounding whitespace, all of which
/// `u32::from_str` or a trimming parse would otherwise let into a label.
fn number(value: &str) -> Option<u32> {
    if value.bytes().all(|byte| byte.is_ascii_digit()) {
        value.parse().ok()
    } else {
        None
    }
}
