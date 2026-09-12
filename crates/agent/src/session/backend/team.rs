use serde_json::Value;
use tracing::trace;

use crate::chat::TeamDecisionRequest;
use crate::codex::app_server;
use crate::session::team_capabilities::{TeamCapabilities, TeamLaunch};
use crate::session::team_recovery::RecoveredTeamTurn;
use crate::session::{AgentKind, Backend, RecoveryIdentity};
use crate::{AgentWorkspace, LaunchConfig};

impl Backend {
    pub fn team_recovered_turns(&self) -> &[RecoveredTeamTurn] {
        match self {
            Self::Codex(session) => session.team_recovered_turns(),

            #[cfg(any(test, feature = "test-support"))]
            Self::Test(backend) => &backend.team_recovered_turns,

            _ => &[],
        }
    }

    pub fn spawn_team(
        kind: AgentKind,
        launch: &LaunchConfig,
        host_catalog: &[LaunchConfig],
        workspace: &AgentWorkspace,
        recovery: Option<RecoveryIdentity>,
        policy: TeamLaunch,
        deliver: impl Fn(Value) + Send + Sync + 'static,
    ) -> Result<Self, String> {
        if recovery
            .as_ref()
            .is_some_and(|identity| identity.kind != kind)
        {
            return Err("The saved Team conversation belongs to another provider.".into());
        }

        if kind == AgentKind::DeepSeek && recovery.is_some() {
            return Err("DeepSeek cannot resume this saved Team conversation. Keep its history and explicitly create a new member.".into());
        }

        if kind == AgentKind::Codex {
            return app_server::Session::spawn_team(
                launch,
                host_catalog,
                workspace,
                recovery.map(|identity| identity.id),
                policy,
                deliver,
                |line| trace!("codex app-server: {line}"),
            )
            .map(Self::Codex);
        }

        Self::spawn(kind, launch, host_catalog, workspace, recovery, deliver)
    }

    pub fn team_capabilities(&self, kind: AgentKind, backend_generation: u64) -> TeamCapabilities {
        match self {
            Self::Codex(session) => session.team_capabilities(backend_generation),
            _ => TeamCapabilities::unverified(kind),
        }
    }

    pub fn respond_team_decision(
        &mut self,
        request: &TeamDecisionRequest,
        accepted: bool,
        explanation: &str,
    ) -> bool {
        match self {
            Self::Codex(session) => session.respond_team_decision(request, accepted, explanation),
            _ => false,
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    use serde_json::json;
    use tempfile::tempdir;

    use crate::chat::Event;
    use crate::session::team_capabilities::TeamLaunch;
    use crate::session::{AgentKind, Backend};
    use crate::{AgentWorkspace, LaunchConfig};

    #[test]
    fn claude_team_uses_the_ordinary_backend() {
        let directory = tempdir().unwrap();

        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/claude/fake-stream-json.cmd");

        let launch = LaunchConfig {
            executable: fixture.to_string_lossy().into_owned(),
            env: vec![(
                "NMT_FAKE_STREAM_LOG".into(),
                directory
                    .path()
                    .join("input.jsonl")
                    .to_string_lossy()
                    .into_owned(),
            )],
            ..LaunchConfig::default()
        };

        let policy = TeamLaunch {
            moderator: false,
            restore_transcript: false,
        };

        let mut backend = Backend::spawn_team(
            AgentKind::Claude,
            &launch,
            &[],
            &AgentWorkspace::default(),
            None,
            policy,
            |_| {},
        )
        .unwrap_or_else(|error| panic!("Claude Team startup failed: {error}"));

        let Backend::Claude(session) = &mut backend else {
            panic!("Claude Team did not start the ordinary backend");
        };

        let events = session.process(json!({
            "type": "control_request",
            "request_id": "write-request",
            "request": {
                "subtype": "can_use_tool",
                "tool_name": "Write",
                "input": {"file_path": "test.txt", "content": "test"}
            }
        }));

        assert!(
            events
                .iter()
                .any(|event| matches!(event, Event::ApprovalRequested { .. }))
        );

        backend.shutdown(Duration::from_secs(2), true).unwrap();
    }
}
