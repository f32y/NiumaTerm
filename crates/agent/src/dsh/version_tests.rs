//! Installed-release checks for DeepSeek integration tests.

use std::time::Duration;

use semver::{Version, VersionReq};

use crate::launcher::{AgentCli, ProcessLimits, run_bounded};

/// The exact release used by the package launchers. The Remote API can change
/// between pre-releases, so other releases retain a compatibility notice.
const SUPPORTED_VERSIONS: &str = "=0.1.5-rc.1";

/// `dsh --version` only has to start Node and print, but a first run on a cold
/// machine still pays for module resolution.
const VERSION_TIMEOUT: Duration = Duration::from_secs(20);

const VERSION_OUTPUT_LIMIT: usize = 8 * 1024;

/// What the installed harness is, relative to what this build supports. An
/// unsupported version does not block a tab: refusing to run against an
/// untested release would make every harness update an outage, and the
/// interface usually keeps working.
#[derive(Clone, Debug, PartialEq, Eq)]
enum VersionSupport {
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
fn describe_version(cli: &AgentCli) -> VersionSupport {
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
fn classify(installed: &Version) -> VersionSupport {
    let requirement = VersionReq::parse(SUPPORTED_VERSIONS)
        .expect("the supported range is a literal in this file");

    // The exact requirement includes the pre-release identifier so a later
    // release candidate or stable release still receives a compatibility notice.
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

#[test]
fn only_the_pinned_release_is_reported_as_supported() {
    assert_eq!(
        classify(&Version::parse("0.1.5-rc.1").unwrap()),
        VersionSupport::Supported,
    );

    for outside in [
        "0.1.0-rc.6",
        "0.1.1-rc.2",
        "0.1.2-rc.0",
        "0.1.2-rc.1",
        "0.1.2",
        "0.1.3",
        "0.1.5-rc.0",
        "0.1.5-rc.2",
        "0.1.5",
        "0.2.0",
        "1.0.0",
    ] {
        assert!(
            matches!(
                classify(&Version::parse(outside).unwrap()),
                VersionSupport::Unsupported { .. }
            ),
            "{outside}"
        );
    }
}

#[cfg(windows)]
#[test]
#[ignore = "resolves the installed harness"]
fn the_installed_release_is_one_this_build_supports() {
    let cli = AgentCli::new("dsh", []);

    assert_eq!(
        describe_version(&cli),
        VersionSupport::Supported,
        "the installed harness is outside {SUPPORTED_VERSIONS}"
    );
}
