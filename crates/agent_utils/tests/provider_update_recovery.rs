#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};
use std::{env, fs};

use nmt_agent_utils::LaunchConfig;
use nmt_agent_utils::chat::Event;
use nmt_agent_utils::claude_code::stream_json;
use nmt_agent_utils::codex::app_server;
use nmt_agent_utils::launcher::AgentCli;
use nmt_agent_utils::update::{
    ClaudeMaintenance, ClaudeReleaseChannel, CodexMaintenance, InstallationKey, ProviderKind,
    ProviderMaintenance, UpdateCoordinator, UpdateError, UpdatePhase,
};
use semver::Version;
use serde_json::Value;
use uuid::Uuid;

const FAKE_AGENT: &str = r#"
$ErrorActionPreference = 'Stop'
$version = if (Test-Path -LiteralPath $env:NMT_FAKE_VERSION_MARKER) { '1.1.0' } else { '1.0.0' }

if ($args.Count -ge 1 -and $args[0] -eq 'doctor') {
    if ($env:NMT_FAKE_PROVIDER -eq 'codex') {
        [Console]::Out.WriteLine(
            '{"schemaVersion":1,"codexVersion":"' + $version +
            '","checks":{"runtime.provenance":{"details":{"install method":"npm (fake)"}},' +
            '"installation":{"details":{"install context":"npm"}},' +
            '"updates.status":{"details":{"latest version":"1.1.0",' +
            '"update action":"fake provider update"}}}}'
        )
    } else {
        [Console]::Out.WriteLine("Running: native ($version)")
        [Console]::Out.WriteLine('Config install method: native')
        [Console]::Out.WriteLine('Auto-updates: disabled')
        [Console]::Out.WriteLine('Auto-update channel: latest')
    }
    exit 0
}

if ($args.Count -ge 1 -and $args[0] -eq '--version') {
    [Console]::Out.WriteLine("$($env:NMT_FAKE_PROVIDER) $version")
    exit 0
}

if ($args.Count -ge 1 -and $args[0] -eq 'update') {
    [IO.File]::AppendAllText($env:NMT_FAKE_UPDATE_LOG, "update$([Environment]::NewLine)")
    [IO.File]::WriteAllText($env:NMT_FAKE_VERSION_MARKER, '1.1.0')
    exit 0
}

[IO.File]::AppendAllText(
    $env:NMT_FAKE_SESSION_LOG,
    (($args -join ' ') + [Environment]::NewLine)
)

if ($env:NMT_FAKE_PROVIDER -eq 'claude') {
    $sessionId = $env:NMT_FAKE_CONVERSATION_ID
    [Console]::Out.WriteLine(
        (@{
            type = 'system'
            subtype = 'init'
            session_id = $sessionId
            model = 'fake-claude'
            permissionMode = 'default'
        } | ConvertTo-Json -Compress)
    )
    while ([Console]::In.ReadLine() -ne $null) {}
    exit 0
}

if ($args.Count -ge 1 -and $args[0] -eq 'app-server') {
    while (($line = [Console]::In.ReadLine()) -ne $null) {
        $request = $line | ConvertFrom-Json
        if ($null -eq $request.id) { continue }

        if ($request.method -eq 'thread/start') {
            $threadId = $env:NMT_FAKE_CONVERSATION_ID
            $result = @{
                thread = @{ id = $threadId; turns = @() }
                model = 'fake-codex'
                approvalPolicy = 'on-request'
                sandbox = @{ type = 'workspace-write' }
                reasoningEffort = 'medium'
                serviceTier = 'priority'
            }
        } elseif ($request.method -eq 'thread/resume') {
            $threadId = [string]$request.params.threadId
            [IO.File]::AppendAllText(
                $env:NMT_FAKE_RESUME_LOG,
                ("$threadId$([Environment]::NewLine)")
            )
            $result = @{
                thread = @{ id = $threadId; turns = @() }
                model = 'fake-codex'
                approvalPolicy = 'on-request'
                sandbox = @{ type = 'workspace-write' }
                reasoningEffort = 'medium'
                serviceTier = 'priority'
            }
        } elseif ($request.method -eq 'model/list' -or $request.method -eq 'thread/list') {
            $result = @{ data = @() }
        } else {
            $result = @{}
        }

        [Console]::Out.WriteLine(
            (@{ jsonrpc = '2.0'; id = [int64]$request.id; result = $result } |
                ConvertTo-Json -Compress -Depth 8)
        )
    }
    exit 0
}

exit 9
"#;

struct FakeAgentFixture {
    root: PathBuf,
    executable: PathBuf,
    version_marker: PathBuf,
    update_log: PathBuf,
    session_log: PathBuf,
    resume_log: PathBuf,
}

impl FakeAgentFixture {
    fn new() -> Self {
        let root = env::temp_dir().join(format!(
            "NiumaTerm provider update recovery {}",
            Uuid::new_v4()
        ));
        fs::create_dir_all(&root).unwrap();
        let script = root.join("fake-agent.ps1");
        let executable = root.join("fake agent.cmd");
        fs::write(&script, FAKE_AGENT).unwrap();
        fs::write(
            &executable,
            "@echo off\r\npwsh.exe -NoProfile -ExecutionPolicy Bypass -File \"%~dp0fake-agent.ps1\" %*\r\nexit /b %ERRORLEVEL%\r\n",
        )
        .unwrap();
        Self {
            root: root.clone(),
            executable,
            version_marker: root.join("installed-version.txt"),
            update_log: root.join("updates.txt"),
            session_log: root.join("sessions.txt"),
            resume_log: root.join("resumes.txt"),
        }
    }

    fn launch(&self, provider: ProviderKind, conversation_id: &str) -> LaunchConfig {
        LaunchConfig {
            executable: self.executable.display().to_string(),
            env: vec![
                (
                    "NMT_FAKE_PROVIDER".into(),
                    match provider {
                        ProviderKind::Claude => "claude",
                        ProviderKind::Codex => "codex",
                    }
                    .into(),
                ),
                (
                    "NMT_FAKE_VERSION_MARKER".into(),
                    display(&self.version_marker),
                ),
                ("NMT_FAKE_UPDATE_LOG".into(), display(&self.update_log)),
                ("NMT_FAKE_SESSION_LOG".into(), display(&self.session_log)),
                ("NMT_FAKE_RESUME_LOG".into(), display(&self.resume_log)),
                ("NMT_FAKE_CONVERSATION_ID".into(), conversation_id.into()),
            ],
            ..LaunchConfig::default()
        }
    }

    fn coordinator(&self) -> UpdateCoordinator {
        UpdateCoordinator::new(self.root.join("update-cache.json"))
    }

    fn update_invocations(&self) -> Vec<String> {
        lines(&self.update_log)
    }
}

impl Drop for FakeAgentFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn display(path: &Path) -> String {
    path.display().to_string()
}

fn lines(path: &Path) -> Vec<String> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

fn installation_key(provider: ProviderKind, launch: &LaunchConfig) -> InstallationKey {
    InstallationKey::derive(
        provider,
        &AgentCli::from_launch(launch, provider.default_executable()),
    )
    .key
}

fn register_available(
    fixture: &FakeAgentFixture,
    provider: ProviderKind,
    launch: &LaunchConfig,
    maintenance: Arc<dyn ProviderMaintenance>,
) -> (UpdateCoordinator, InstallationKey) {
    let coordinator = fixture.coordinator();
    let key = coordinator.register(
        provider,
        AgentCli::from_launch(launch, provider.default_executable()),
        maintenance,
    );
    let status = coordinator.check(&key, true).unwrap();
    assert!(
        status.update_available(),
        "unexpected probe result: {status:?}"
    );
    coordinator.begin_update(&key).unwrap();
    (coordinator, key)
}

/// How long a fake agent has to reach its first event. The agent is a
/// PowerShell process, so this covers interpreter startup rather than any work
/// the session does, and the whole suite runs alongside every other test binary
/// in the workspace.
const READY_TIMEOUT: Duration = Duration::from_secs(30);

/// The next message from the agent, or `None` once `deadline` passes.
///
/// A single quiet interval is not a failure. Startup here measures ~400 ms on
/// an idle machine and several seconds when the rest of the workspace's test
/// binaries are running, so only the deadline decides how long to wait; a
/// receive window that gave up on its own would make the deadline decorative.
fn next_message(receiver: &Receiver<Value>, deadline: Instant) -> Option<Value> {
    loop {
        let remaining = deadline.checked_duration_since(Instant::now())?;
        match receiver.recv_timeout(remaining.min(Duration::from_millis(250))) {
            Ok(message) => return Some(message),
            Err(RecvTimeoutError::Timeout) => continue,
            // The sender lives with the reader thread feeding it, so a closed
            // channel means the agent exited and no later message can arrive.
            // Waiting out the deadline would only delay the same failure and
            // report it as a timeout rather than as the exit it is.
            Err(RecvTimeoutError::Disconnected) => {
                panic!("the fake agent exited before its session became ready")
            }
        }
    }
}

fn wait_for_claude_ready(session: &mut stream_json::Session, receiver: &Receiver<Value>) -> String {
    let deadline = Instant::now() + READY_TIMEOUT;
    while let Some(message) = next_message(receiver, deadline) {
        if session
            .process(message)
            .iter()
            .any(|event| matches!(event, Event::Ready(_)))
        {
            return session.session_id().unwrap().to_string();
        }
    }
    panic!("fake Claude session did not become ready");
}

fn wait_for_codex_ready(session: &mut app_server::Session, receiver: &Receiver<Value>) -> String {
    let deadline = Instant::now() + READY_TIMEOUT;
    while let Some(message) = next_message(receiver, deadline) {
        if session
            .process(message)
            .iter()
            .any(|event| matches!(event, Event::Ready(_)))
        {
            return session.thread_id().unwrap().to_string();
        }
    }
    panic!("fake Codex session did not become ready");
}

struct FixedClaudeRelease;

impl ClaudeReleaseChannel for FixedClaudeRelease {
    fn latest(&self, _: &str) -> Result<Version, UpdateError> {
        Ok(Version::new(1, 1, 0))
    }
}

#[test]
fn one_claude_update_restores_multiple_sessions_in_place() {
    let fixture = FakeAgentFixture::new();
    let ids = [
        "10000000-0000-4000-8000-000000000001",
        "10000000-0000-4000-8000-000000000002",
    ];
    let launches = ids.map(|id| fixture.launch(ProviderKind::Claude, id));
    assert_eq!(
        installation_key(ProviderKind::Claude, &launches[0]),
        installation_key(ProviderKind::Claude, &launches[1])
    );

    let mut sessions = Vec::new();
    for launch in &launches {
        let (sender, receiver) = mpsc::channel();
        let mut session = stream_json::Session::spawn(
            launch,
            None,
            None,
            move |value| {
                let _ = sender.send(value);
            },
            |_| {},
        )
        .unwrap();
        let id = wait_for_claude_ready(&mut session, &receiver);
        sessions.push((session, id));
    }

    let maintenance: Arc<dyn ProviderMaintenance> =
        Arc::new(ClaudeMaintenance::new(FixedClaudeRelease));
    let (coordinator, key) =
        register_available(&fixture, ProviderKind::Claude, &launches[0], maintenance);
    for (session, _) in &mut sessions {
        session.shutdown(Duration::from_secs(5), false).unwrap();
    }
    coordinator.run_vendor_update(&key).unwrap();
    let verified = coordinator.verify(&key).unwrap();

    for ((_, id), launch) in sessions.into_iter().zip(&launches) {
        let (sender, receiver) = mpsc::channel();
        let mut resumed = stream_json::Session::spawn(
            launch,
            None,
            Some(id.clone()),
            move |value| {
                let _ = sender.send(value);
            },
            |_| {},
        )
        .unwrap();
        assert_eq!(wait_for_claude_ready(&mut resumed, &receiver), id);
        resumed.shutdown(Duration::from_secs(5), false).unwrap();
    }

    coordinator.finish_update(&key, Some(verified), None, 0);
    assert_eq!(fixture.update_invocations(), ["update"]);
    assert_eq!(
        coordinator.snapshot(&key).unwrap().state.phase,
        UpdatePhase::Updated
    );
    let session_invocations = lines(&fixture.session_log);
    for id in ids {
        assert!(
            session_invocations
                .iter()
                .any(|line| line.contains(&format!("--resume {id}")))
        );
    }
}

#[test]
fn one_codex_update_restores_multiple_threads_without_starting_new_ones() {
    let fixture = FakeAgentFixture::new();
    let ids = ["thr_retained_1", "thr_retained_2"];
    let launches = ids.map(|id| fixture.launch(ProviderKind::Codex, id));
    assert_eq!(
        installation_key(ProviderKind::Codex, &launches[0]),
        installation_key(ProviderKind::Codex, &launches[1])
    );

    let mut sessions = Vec::new();
    for launch in &launches {
        let (sender, receiver) = mpsc::channel();
        let mut session = app_server::Session::spawn(
            launch,
            None,
            move |value| {
                let _ = sender.send(value);
            },
            |_| {},
        )
        .unwrap();
        let id = wait_for_codex_ready(&mut session, &receiver);
        sessions.push((session, id));
    }

    let maintenance: Arc<dyn ProviderMaintenance> = Arc::new(CodexMaintenance);
    let (coordinator, key) =
        register_available(&fixture, ProviderKind::Codex, &launches[0], maintenance);
    for (session, _) in &mut sessions {
        session.shutdown(Duration::from_secs(5), false).unwrap();
    }
    coordinator.run_vendor_update(&key).unwrap();
    let verified = coordinator.verify(&key).unwrap();

    for ((_, id), launch) in sessions.into_iter().zip(&launches) {
        let (sender, receiver) = mpsc::channel();
        let mut resumed = app_server::Session::spawn_resuming(
            launch,
            None,
            id.clone(),
            true,
            move |value| {
                let _ = sender.send(value);
            },
            |_| {},
        )
        .unwrap();
        assert_eq!(wait_for_codex_ready(&mut resumed, &receiver), id);
        resumed.shutdown(Duration::from_secs(5), false).unwrap();
    }

    coordinator.finish_update(&key, Some(verified), None, 0);
    assert_eq!(fixture.update_invocations(), ["update"]);
    assert_eq!(lines(&fixture.resume_log), ids);
    assert_eq!(
        coordinator.snapshot(&key).unwrap().state.phase,
        UpdatePhase::Updated
    );
}
