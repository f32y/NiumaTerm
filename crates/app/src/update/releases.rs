//! Asking GitHub what the selected channel has published, and deciding whether
//! it supersedes what is running.

use std::time::Duration;

use nmt_config::update::UpdateChannel;
use nmt_version::Version;
use reqwest::blocking::Client;
use serde::Deserialize;

use crate::update::APP_VERSION;

/// One request answers both channels: the list is newest first, and each entry
/// says whether it is a prerelease. Thirty entries reach well past the newest
/// of either channel even when one of them is publishing daily.
const RELEASES_URL: &str = "https://api.github.com/repos/f32y/NiumaTerm/releases?per_page=30";

const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// GitHub serves a few kilobytes per release. The cap is generous enough that
/// only a response that is not the releases list can reach it.
const MAX_RESPONSE_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CheckError {
    /// The request never produced a response to read, including the rate limit
    /// GitHub applies per address to unauthenticated callers.
    Unreachable,
    /// A response arrived but was not the releases list.
    Unreadable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Release {
    pub(crate) label: String,
    pub(crate) page_url: String,
}

#[derive(Deserialize)]
struct ReleaseEntry {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
}

pub(crate) fn latest(channel: UpdateChannel) -> Result<Option<Release>, CheckError> {
    let client = Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|_| CheckError::Unreachable)?;

    // GitHub answers an unauthenticated request without a user agent with 403,
    // so the header is required rather than merely polite.
    let response = client
        .get(RELEASES_URL)
        .header("User-Agent", user_agent())
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .map_err(|_| CheckError::Unreachable)?;

    if !response.status().is_success() {
        return Err(CheckError::Unreachable);
    }
    if response.content_length().unwrap_or(0) > MAX_RESPONSE_BYTES {
        return Err(CheckError::Unreadable);
    }

    let body = response.text().map_err(|_| CheckError::Unreadable)?;

    select(&body, channel)
}

/// Split from the request so the selection can be exercised against a recorded
/// response rather than the live releases page.
pub(crate) fn select(body: &str, channel: UpdateChannel) -> Result<Option<Release>, CheckError> {
    let entries =
        serde_json::from_str::<Vec<ReleaseEntry>>(body).map_err(|_| CheckError::Unreadable)?;

    Ok(newest_in_channel(&entries, channel))
}

fn user_agent() -> String {
    format!("NiumaTerm/{APP_VERSION}")
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
        })
}

fn channel_of(version: &Version) -> UpdateChannel {
    match version {
        Version::Release { .. } => UpdateChannel::Stable,
        Version::Nightly { .. } => UpdateChannel::Nightly,
    }
}

/// Whether `candidate` is worth offering over `current`.
pub(crate) fn supersedes(current: &Version, candidate: &Version) -> bool {
    match (current, candidate) {
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
        // The channels moved. Whatever the chosen one publishes is what the
        // user asked to run, even when its version reads as older.
        _ => true,
    }
}
