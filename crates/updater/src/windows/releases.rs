//! Asking the published manifest what the selected channel holds, and deciding
//! whether it supersedes what is running.

#[cfg(test)]
#[path = "releases_tests.rs"]
mod tests;

use std::slice::from_ref;
use std::time::Duration;

use nmt_config::update::UpdateChannel;
use nmt_version::Version;
use reqwest::blocking::Client;
use serde::Deserialize;

use crate::windows::Status;

/// Rendered by CI from GitHub's own "latest", the newest release that is
/// neither a draft nor a prerelease, which is exactly the stable channel.
/// Giving it a document of its own keeps a stable release findable however many
/// nightlies were published after it; a page of the full list cannot promise
/// that, since nightlies published daily push a months-old release off it.
///
/// Served as a static file rather than read from the GitHub API, which allows
/// sixty unauthenticated requests an hour per address. Users who share an
/// outbound address behind carrier or corporate NAT exhaust that between them,
/// and the check then fails for a reason they can neither see nor fix.
const LATEST_RELEASE_URL: &str = "https://niumaterm-updates.f32.io/windows/stable.json";

/// The nightly channel scans a list instead, rendered from the newest thirty
/// releases and ordered newest first. Thirty entries reach past the newest
/// nightly unless the stable channel out-publishes it by that many in a row.
const RELEASES_URL: &str = "https://niumaterm-updates.f32.io/windows/nightly.json";

/// Where release assets are served from: a Worker that caches this
/// repository's downloads at Cloudflare's edge, because a client on a poor path
/// to GitHub pays that cost on every update.
///
/// The digest comes through the same front as the archive it describes. Leaving
/// it on GitHub would keep an update dependent on reaching GitHub, which is the
/// condition this front exists to work around: the archive would arrive in
/// seconds and the check behind it would time out. What that costs is a host
/// whose control is enough to serve an archive together with a digest matching
/// it, so the two no longer corroborate each other.
///
/// An asset URL arrives in the fetched manifest, so following one unchecked
/// would let whoever serves that document point the download at a host nobody
/// chose. Only the prefix is pinned, because the path below it names the tag
/// and the file.
pub(super) const DOWNLOAD_URL_PREFIX: &str = "https://niumaterm-downloads.f32.io/";

const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// The manifest holds a few hundred bytes per release. The cap is generous
/// enough that only a response other than the manifest can reach it.
const MAX_RESPONSE_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckError {
    /// The request never produced a response to read.
    Unreachable,
    /// A response arrived but was not the releases list.
    Unreadable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub label: String,
    pub page_url: String,
    pub(super) assets: Vec<Asset>,

    /// When the channel published it, as `yyyymmdd`, which is the only thing a
    /// release tag and a nightly label can be ordered by across channels. Left
    /// unset for a response that carried no timestamp this could read.
    pub(super) published: Option<u32>,
}

/// One file published with a release.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub(super) struct Asset {
    pub(super) name: String,
    #[serde(rename = "browser_download_url")]
    pub(super) url: String,
}

#[derive(Deserialize)]
struct ReleaseEntry {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,

    /// Absent from the recorded responses the selection is tested against, and
    /// from a release published before anything was attached to it.
    #[serde(default)]
    assets: Vec<Asset>,

    /// `2026-08-14T09:12:33Z`. Null for a draft, which never reaches a
    /// comparison, and absent from the recorded responses.
    #[serde(default)]
    published_at: Option<String>,
}

/// One check with the channel and running version captured before it starts.
pub struct Check {
    pub(super) channel: UpdateChannel,
    pub(super) version: &'static str,
}

impl Check {
    /// Fetch and compare the published release on a worker thread.
    pub fn run(self) -> CheckedRelease {
        let found = latest(self.channel, self.version);

        CheckedRelease {
            channel: self.channel,
            status: outcome(found, self.version),
        }
    }
}

/// Retains the requested channel so a late result cannot replace a result for
/// a channel the user selected while the request was running.
pub struct CheckedRelease {
    pub(super) channel: UpdateChannel,
    pub(super) status: Status,
}

fn latest(channel: UpdateChannel, version: &str) -> Result<Option<Release>, CheckError> {
    let client = Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|_| CheckError::Unreachable)?;

    match channel {
        UpdateChannel::Stable => select_latest(&get(&client, LATEST_RELEASE_URL, version)?),
        UpdateChannel::Nightly => select(&get(&client, RELEASES_URL, version)?, channel),
    }
}

fn get(client: &Client, url: &str, version: &str) -> Result<String, CheckError> {
    // The agent string names the build doing the asking, which is what makes
    // the serving edge's log useful when a release turns out to be unreadable
    // for one version and fine for the rest.
    let response = client
        .get(url)
        .header("User-Agent", user_agent(version))
        .send()
        .map_err(|_| CheckError::Unreachable)?;

    if !response.status().is_success() {
        return Err(CheckError::Unreachable);
    }

    if response.content_length().unwrap_or(0) > MAX_RESPONSE_BYTES {
        return Err(CheckError::Unreadable);
    }

    response.text().map_err(|_| CheckError::Unreadable)
}

/// Split from the request so the selection can be exercised against a recorded
/// response rather than the live releases page.
fn select(body: &str, channel: UpdateChannel) -> Result<Option<Release>, CheckError> {
    let entries =
        serde_json::from_str::<Vec<ReleaseEntry>>(body).map_err(|_| CheckError::Unreadable)?;

    Ok(newest_in_channel(&entries, channel))
}

/// The single entry the stable manifest holds. It is still checked against the
/// stable channel: the manifest promises the newest published non-prerelease,
/// leaving open whether its tag is one this build can be compared against.
fn select_latest(body: &str) -> Result<Option<Release>, CheckError> {
    let entry = serde_json::from_str::<ReleaseEntry>(body).map_err(|_| CheckError::Unreadable)?;

    Ok(newest_in_channel(from_ref(&entry), UpdateChannel::Stable))
}

pub(super) fn user_agent(version: &str) -> String {
    format!("NiumaTerm/{version}")
}

fn outcome(found: Result<Option<Release>, CheckError>, version: &str) -> Status {
    let published = match found {
        Ok(Some(published)) => published,
        Ok(None) => return Status::NothingPublished,
        Err(error) => return Status::Failed(error),
    };

    // An unreadable running label cannot prove that the published build is
    // older, so that build remains available for installation.
    match Version::parse(version) {
        Some(current) if !supersedes(&current, &published) => Status::UpToDate,
        _ => Status::Available(published),
    }
}

/// The list arrives newest first, so the first entry whose tag belongs to the
/// channel is that channel's newest. Tags that predate the current naming, and
/// tags that mark something other than a release, parse to nothing and are
/// skipped rather than guessed at.
fn newest_in_channel(entries: &[ReleaseEntry], channel: UpdateChannel) -> Option<Release> {
    entries
        .iter()
        .filter(|entry| !entry.draft)
        .find(|entry| {
            let published = match Version::parse(&entry.tag_name) {
                Some(version) => version,
                None => return false,
            };

            channel_of(&published) == channel
                && entry.prerelease == (channel == UpdateChannel::Nightly)
        })
        .map(|entry| Release {
            label: entry.tag_name.clone(),
            page_url: entry.html_url.clone(),
            assets: entry.assets.clone(),
            published: entry.published_at.as_deref().and_then(publish_date),
        })
}

/// `2026-08-14T09:12:33Z` reduced to `20260814`, the form a nightly label
/// already carries its date in, so the two order against each other as numbers.
/// Anything else yields nothing rather than a date built from whatever happened
/// to sit at those offsets.
fn publish_date(timestamp: &str) -> Option<u32> {
    let date = timestamp.get(..10)?;
    let digits = format!("{}{}{}", date.get(..4)?, date.get(5..7)?, date.get(8..10)?);

    digits
        .bytes()
        .all(|byte| byte.is_ascii_digit())
        .then(|| digits.parse().ok())?
}

fn channel_of(version: &Version) -> UpdateChannel {
    match version {
        Version::Release { .. } => UpdateChannel::Stable,
        Version::Nightly { .. } => UpdateChannel::Nightly,
    }
}

/// Whether `candidate` is worth offering over `current`.
fn supersedes(current: &Version, candidate: &Release) -> bool {
    // A tag this build cannot read is offered rather than hidden: the channel
    // published it, and a name nothing here understands is no evidence that it
    // is behind.
    let Some(published) = Version::parse(&candidate.label) else {
        return true;
    };

    match (current, &published) {
        (
            Version::Release {
                major: current_major,
                minor: current_minor,
                patch: current_patch,
            },
            Version::Release {
                major,
                minor,
                patch,
            },
        ) => (major, minor, patch) > (current_major, current_minor, current_patch),
        (
            Version::Nightly {
                date: current_date,
                commit: current_commit,
            },
            Version::Nightly { date, commit },
        ) => {
            // A nightly rebuilt later the same day carries that day's date and
            // a different revision, and is the one to run: the published list
            // is ordered by when each release was cut, so reaching this entry
            // at all means it is the newest the channel has.
            date > current_date || (date == current_date && commit != current_commit)
        }
        // A nightly is cut from the tip of the development line, so a release
        // published before that build existed is behind it however its number
        // reads, and installing it would move the installation backwards. One
        // published on a later day carries work the nightly cannot, which is
        // what lets a months-old nightly move to the stable channel.
        //
        // A nightly label dates a build to the day, and a release is routinely
        // cut from the same day's tree, so a same-day release is exactly the
        // ambiguous case and is left alone: a release already published when
        // that day's build was made is the downgrade being avoided, and the
        // next release lands on a later day. For the same reason a release with
        // no publishing date read from the response is treated as behind.
        (Version::Nightly { date, .. }, Version::Release { .. }) => candidate
            .published
            .is_some_and(|published| published > *date),
        // The other direction needs no such evidence: the nightly channel is
        // only ever cut from the tip, so what it publishes is never behind a
        // release.
        (Version::Release { .. }, Version::Nightly { .. }) => true,
    }
}
