use crate::claude_code::update::*;

const CLAUDE_DOCTOR: &str = "Claude Code doctor\n\nRunning: native (2.1.222)\nConfig install method: native\nAuto-updates: enabled\nAuto-update channel: latest\n";

#[test]
fn doctor_parses_bounded_known_fields() {
    let status = parse_claude_doctor(CLAUDE_DOCTOR).unwrap();
    assert_eq!(status.current, Some(Version::new(2, 1, 222)));
    assert_eq!(status.install_method.as_deref(), Some("native"));
    assert_eq!(status.channel.as_deref(), Some("latest"));
    assert!(status.can_update);

    let unknown = CLAUDE_DOCTOR.replace("latest", "canary");
    let status = parse_claude_doctor(&unknown).unwrap();
    assert_eq!(status.channel.as_deref(), Some("canary"));
}

struct FakeReleases(Result<Version, UpdateError>);

impl ClaudeReleaseChannel for FakeReleases {
    fn latest(&self, _channel: &str) -> Result<Version, UpdateError> {
        self.0.clone()
    }
}

#[test]
fn release_client_contract_rejects_unknown_channels_and_invalid_versions() {
    let client = HttpClaudeReleaseChannel::with_base_url("http://127.0.0.1:9").unwrap();
    assert_eq!(
        client.latest("canary").unwrap_err().kind,
        UpdateErrorKind::Unsupported
    );
    assert!(parse_strict_version("2.1.222", "version").is_ok());
    assert!(parse_strict_version("v2.1.222", "version").is_err());
    let fake = FakeReleases(Ok(Version::new(2, 1, 223)));
    assert_eq!(fake.latest("latest").unwrap(), Version::new(2, 1, 223));
}
