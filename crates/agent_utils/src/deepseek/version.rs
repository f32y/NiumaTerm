//! Which `dsh` releases this build was written against.
//!
//! The interface carries no protocol version field, and the host's own
//! `host.describe` reports the web application's version (`0.0.1`) rather than
//! the harness release, so it cannot serve as the gate. The installed package
//! version is what this checks instead.

use std::time::Duration;

use semver::{Version, VersionReq};

use crate::launcher::{AgentCli, ProcessLimits, run_bounded};

/// The range this build has been exercised against. `dsh` is pre-release with
/// an explicit expectation of breaking changes, so this is a statement about
/// what was tested rather than a guarantee about what works.
pub const SUPPORTED_VERSIONS: &str = ">=0.1.0-rc.6, <0.2.0";

/// `dsh --version` only has to start Node and print, but a first run on a cold
/// machine still pays for module resolution.
const VERSION_TIMEOUT: Duration = Duration::from_secs(20);
const VERSION_OUTPUT_LIMIT: usize = 8 * 1024;

/// What the installed harness is, relative to what this build supports. An
/// unsupported version does not block a tab: refusing to run against an
/// untested release would make every harness update an outage, and the
/// interface usually keeps working.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VersionSupport {
    Supported,
    /// Installed and readable, but outside the tested range.
    Unsupported {
        installed: String,
        supported: String,
    },
    /// The version could not be read at all. Reported, but not treated as a
    /// reason to refuse: a harness that answers its interface works whether or
    /// not it can describe itself.
    Unknown(String),
}

/// Ask the installed harness what it is.
pub fn describe_version(cli: &AgentCli) -> VersionSupport {
    let run = match run_bounded(
        cli,
        ["--version"],
        ProcessLimits::new(VERSION_TIMEOUT, VERSION_OUTPUT_LIMIT),
    ) {
        Ok(run) => run,
        Err(error) => return VersionSupport::Unknown(error.to_string()),
    };

    if !run.success() {
        return VersionSupport::Unknown(run.diagnostic());
    }

    // The redacted view can rewrite any configured environment value it finds,
    // and a short credential could collide with a version string.
    match parse_version(run.stdout_for_parsing()) {
        Some(installed) => classify(&installed),
        None => VersionSupport::Unknown(run.diagnostic()),
    }
}

/// Compare a known version against the supported range. Separate from the
/// process run so the decision can be exercised without launching anything.
pub(crate) fn classify(installed: &Version) -> VersionSupport {
    let requirement = VersionReq::parse(SUPPORTED_VERSIONS)
        .expect("the supported range is a literal in this file");

    // Pre-release versions only satisfy a requirement whose own bound is a
    // pre-release of the same triple, which is exactly how the tested lower
    // bound is written. Matching therefore means what it says here.
    if requirement.matches(installed) {
        VersionSupport::Supported
    } else {
        VersionSupport::Unsupported {
            installed: installed.to_string(),
            supported: SUPPORTED_VERSIONS.to_string(),
        }
    }
}

/// The output is a bare version line, but a warning printed before it would
/// otherwise make the whole run unreadable, so each line is tried in turn.
fn parse_version(output: &str) -> Option<Version> {
    output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .find_map(|line| Version::parse(line.trim_start_matches('v')).ok())
}
