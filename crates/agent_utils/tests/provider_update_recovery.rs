#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};
use std::{env, fs, thread};

use nmt_agent_utils::chat::Event;
use nmt_agent_utils::claude_code::stream_json;
use nmt_agent_utils::codex::app_server;
use nmt_agent_utils::launcher::AgentCli;
use nmt_agent_utils::update::{
    ClaudeMaintenance, ClaudeReleaseChannel, CodexMaintenance, InstallationKey, ProviderKind,
    ProviderMaintenance, UpdateCoordinator, UpdateError, UpdatePhase,
};
use nmt_agent_utils::{AgentWorkspace, CodexProviderConfig, LaunchConfig};
use parking_lot::Mutex;
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
    if ($env:NMT_FAKE_UPDATE_RESULT -eq 'failed') { exit 17 }
    if ($env:NMT_FAKE_UPDATE_RESULT -eq 'unchanged') { exit 0 }
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
    $threadStartCount = 0
    while (($line = [Console]::In.ReadLine()) -ne $null) {
        [IO.File]::AppendAllText(
            $env:NMT_FAKE_REQUEST_LOG,
            ("$line$([Environment]::NewLine)")
        )
        $request = $line | ConvertFrom-Json
        if ($null -ne $request.method) {
            [IO.File]::AppendAllText(
                $env:NMT_FAKE_RPC_LOG,
                ("$($request.method)$([Environment]::NewLine)")
            )
        }
        if ($null -eq $request.id) { continue }

        if ($request.method -eq 'initialize' -and $env:NMT_FAKE_FAIL_INITIALIZE -eq '1') {
            Start-Sleep -Milliseconds 200
            [Console]::Out.WriteLine(
                (@{
                    jsonrpc = '2.0'
                    id = [int64]$request.id
                    error = @{ code = -32000; message = 'fake initialize failure' }
                } | ConvertTo-Json -Compress -Depth 8)
            )
            exit 72
        }

        if ($request.method -eq 'thread/start') {
            $threadStartCount++
            $threadId = [string]$request.params.model
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
            if ($threadId -eq $env:NMT_FAKE_FAIL_RESUME_ID) {
                [Console]::Out.WriteLine(
                    (@{
                        jsonrpc = '2.0'
                        id = [int64]$request.id
                        error = @{ code = -32000; message = 'fake resume failure' }
                    } | ConvertTo-Json -Compress -Depth 8)
                )
                continue
            }
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
        if (
            $request.method -eq 'thread/start' -and
            [int]$env:NMT_FAKE_EXIT_AFTER_THREAD_STARTS -gt 0 -and
            $threadStartCount -ge [int]$env:NMT_FAKE_EXIT_AFTER_THREAD_STARTS
        ) {
            exit 71
        }
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
    rpc_log: PathBuf,
    request_log: PathBuf,
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
            rpc_log: root.join("rpc.txt"),
            request_log: root.join("requests.jsonl"),
        }
    }

    fn launch(&self, provider: ProviderKind, conversation_id: &str) -> LaunchConfig {
        let mut env = vec![
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
            ("NMT_FAKE_RPC_LOG".into(), display(&self.rpc_log)),
            ("NMT_FAKE_REQUEST_LOG".into(), display(&self.request_log)),
        ];
        if provider == ProviderKind::Claude {
            env.push(("NMT_FAKE_CONVERSATION_ID".into(), conversation_id.into()));
        }
        LaunchConfig {
            executable: self.executable.display().to_string(),
            model: (provider == ProviderKind::Codex).then(|| conversation_id.to_string()),
            env,
            ..LaunchConfig::default()
        }
    }

    fn coordinator(&self) -> UpdateCoordinator {
        UpdateCoordinator::new(self.root.join("update-cache.json"))
    }

    fn update_invocations(&self) -> Vec<String> {
        lines(&self.update_log)
    }

    fn rpc_methods(&self) -> Vec<String> {
        lines(&self.rpc_log)
    }
}

static CODEX_TEST_LOCK: Mutex<()> = Mutex::new(());

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

fn wait_for_rpc_count(fixture: &FakeAgentFixture, method: &str, expected: usize) {
    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        let count = fixture
            .rpc_methods()
            .iter()
            .filter(|candidate| candidate.as_str() == method)
            .count();
        if count >= expected {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {method}");
        thread::sleep(Duration::from_millis(5));
    }
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

fn wait_for_codex_host_exit(session: &mut app_server::Session, receiver: &Receiver<Value>) {
    let deadline = Instant::now() + READY_TIMEOUT;
    while let Some(message) = next_message(receiver, deadline) {
        if session
            .process(message)
            .iter()
            .any(|event| matches!(event, Event::HostExited { .. }))
        {
            return;
        }
    }
    panic!("fake Codex host did not report its exit");
}

fn wait_for_codex_resume_failure(session: &mut app_server::Session, receiver: &Receiver<Value>) {
    let deadline = Instant::now() + READY_TIMEOUT;
    while let Some(message) = next_message(receiver, deadline) {
        if session.process(message).iter().any(|event| {
            matches!(
                event,
                Event::Error {
                    fatal: true,
                    message
                } if message.contains("fake resume failure")
            )
        }) {
            return;
        }
    }
    panic!("fake Codex resume did not fail as requested");
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
            &AgentWorkspace::default(),
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
            &AgentWorkspace::default(),
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
    let _guard = CODEX_TEST_LOCK.lock();
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
            &launches,
            &AgentWorkspace::default(),
            move |value| {
                let _ = sender.send(value);
            },
            |_| {},
        )
        .unwrap();
        let id = wait_for_codex_ready(&mut session, &receiver);
        sessions.push((session, id));
    }
    assert_eq!(
        fixture
            .rpc_methods()
            .iter()
            .filter(|method| method.as_str() == "initialize")
            .count(),
        1
    );
    assert_eq!(
        fixture
            .rpc_methods()
            .iter()
            .filter(|method| method.as_str() == "thread/start")
            .count(),
        2
    );

    let maintenance: Arc<dyn ProviderMaintenance> = Arc::new(CodexMaintenance);
    let (coordinator, key) =
        register_available(&fixture, ProviderKind::Codex, &launches[0], maintenance);
    for (session, _) in &mut sessions {
        session.shutdown(Duration::from_secs(5), false).unwrap();
    }
    coordinator.run_vendor_update(&key).unwrap();
    let verified = coordinator.verify(&key).unwrap();

    let mut resumed_sessions = Vec::new();
    for ((_, id), launch) in sessions.into_iter().zip(&launches) {
        let (sender, receiver) = mpsc::channel();
        let mut resumed = app_server::Session::spawn_resuming(
            launch,
            &launches,
            &AgentWorkspace::default(),
            id.clone(),
            true,
            move |value| {
                let _ = sender.send(value);
            },
            |_| {},
        )
        .unwrap();
        assert_eq!(wait_for_codex_ready(&mut resumed, &receiver), id);
        resumed_sessions.push(resumed);
    }
    assert_eq!(
        fixture
            .rpc_methods()
            .iter()
            .filter(|method| method.as_str() == "initialize")
            .count(),
        2
    );
    assert_eq!(
        fixture
            .rpc_methods()
            .iter()
            .filter(|method| method.as_str() == "thread/resume")
            .count(),
        2
    );
    for resumed in &mut resumed_sessions {
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

#[test]
fn failed_and_unchanged_codex_updates_still_restore_all_threads() {
    let _guard = CODEX_TEST_LOCK.lock();
    for (outcome, expected_phase) in [
        ("failed", UpdatePhase::Failed),
        ("unchanged", UpdatePhase::Unchanged),
    ] {
        let fixture = FakeAgentFixture::new();
        let ids = ["thread-outcome-a", "thread-outcome-b"];
        let mut launches = ids.map(|id| fixture.launch(ProviderKind::Codex, id));
        for launch in &mut launches {
            launch
                .env
                .push(("NMT_FAKE_UPDATE_RESULT".into(), outcome.into()));
        }

        let mut sessions = Vec::new();
        for launch in &launches {
            let (sender, receiver) = mpsc::channel();
            let mut session = app_server::Session::spawn(
                launch,
                &launches,
                &AgentWorkspace::default(),
                move |value| {
                    let _ = sender.send(value);
                },
                |_| {},
            )
            .expect("session should start");
            let id = wait_for_codex_ready(&mut session, &receiver);
            sessions.push((session, id));
        }

        let maintenance: Arc<dyn ProviderMaintenance> = Arc::new(CodexMaintenance);
        let (coordinator, key) =
            register_available(&fixture, ProviderKind::Codex, &launches[0], maintenance);
        for (session, _) in &mut sessions {
            session.shutdown(Duration::from_secs(5), false).unwrap();
        }
        let update_error = coordinator.run_vendor_update(&key).err();
        let verified = update_error.is_none().then(|| {
            coordinator
                .verify(&key)
                .expect("unchanged install should verify")
        });

        let mut resumed_sessions = Vec::new();
        for ((_, id), launch) in sessions.into_iter().zip(&launches) {
            let (sender, receiver) = mpsc::channel();
            let mut resumed = app_server::Session::spawn_resuming(
                launch,
                &launches,
                &AgentWorkspace::default(),
                id.clone(),
                true,
                move |value| {
                    let _ = sender.send(value);
                },
                |_| {},
            )
            .expect("restoration should start");
            assert_eq!(wait_for_codex_ready(&mut resumed, &receiver), id);
            resumed_sessions.push(resumed);
        }

        coordinator.finish_update(&key, verified, update_error, 0);
        assert_eq!(
            coordinator.snapshot(&key).unwrap().state.phase,
            expected_phase
        );
        assert_eq!(
            fixture
                .rpc_methods()
                .iter()
                .filter(|method| method.as_str() == "initialize")
                .count(),
            2
        );
        assert_eq!(
            fixture
                .rpc_methods()
                .iter()
                .filter(|method| method.as_str() == "thread/resume")
                .count(),
            2
        );
        for session in &mut resumed_sessions {
            session.shutdown(Duration::from_secs(5), false).unwrap();
        }
    }
}

#[test]
fn one_codex_host_starts_threads_for_two_custom_gateways() {
    let _guard = CODEX_TEST_LOCK.lock();
    let fixture = FakeAgentFixture::new();
    let mut first = fixture.launch(ProviderKind::Codex, "model-a");
    first.provider = Some(CodexProviderConfig {
        id: "provider-a".into(),
        name: "Provider A".into(),
        base_url: "https://gateway-a.example/v1".into(),
        api_key_env: Some("NMT_CODEX_KEY_A".into()),
    });
    first
        .env
        .push(("NMT_CODEX_KEY_A".into(), "secret-a".into()));
    let mut second = fixture.launch(ProviderKind::Codex, "model-b");
    second.provider = Some(CodexProviderConfig {
        id: "provider-b".into(),
        name: "Provider B".into(),
        base_url: "https://gateway-b.example/v1".into(),
        api_key_env: Some("NMT_CODEX_KEY_B".into()),
    });
    second
        .env
        .push(("NMT_CODEX_KEY_B".into(), "secret-b".into()));
    let launches = [first, second];
    let workspaces = [
        AgentWorkspace::single(Some("C:/WorkspaceA".into())),
        AgentWorkspace::single(Some("C:/WorkspaceB".into())),
    ];

    let mut sessions = Vec::new();
    for (launch, workspace) in launches.iter().zip(&workspaces) {
        let (sender, receiver) = mpsc::channel();
        let mut session = app_server::Session::spawn(
            launch,
            &launches,
            workspace,
            move |value| {
                let _ = sender.send(value);
            },
            |_| {},
        )
        .expect("custom gateway session should start");
        let _ = wait_for_codex_ready(&mut session, &receiver);
        sessions.push(session);
    }

    let requests: Vec<Value> = lines(&fixture.request_log)
        .into_iter()
        .filter_map(|line| serde_json::from_str(&line).ok())
        .filter(|request: &Value| request["method"] == "thread/start")
        .collect();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0]["params"]["modelProvider"], "provider-a");
    assert_eq!(requests[0]["params"]["cwd"], "C:/WorkspaceA");
    assert_eq!(
        requests[0]["params"]["config"]["model_providers.provider-a"]["base_url"],
        "https://gateway-a.example/v1"
    );
    assert_eq!(
        requests[0]["params"]["config"]["model_providers.provider-a"]["env_key"],
        "NMT_CODEX_KEY_A"
    );
    assert_eq!(requests[1]["params"]["modelProvider"], "provider-b");
    assert_eq!(requests[1]["params"]["cwd"], "C:/WorkspaceB");
    let skill_requests: Vec<Value> = lines(&fixture.request_log)
        .into_iter()
        .filter_map(|line| serde_json::from_str(&line).ok())
        .filter(|request: &Value| request["method"] == "skills/list")
        .collect();
    assert_eq!(skill_requests.len(), 2);
    assert_eq!(skill_requests[0]["params"]["cwds"][0], "C:/WorkspaceA");
    assert_eq!(skill_requests[1]["params"]["cwds"][0], "C:/WorkspaceB");
    let request_text = fs::read_to_string(&fixture.request_log).unwrap();
    assert!(!request_text.contains("secret-a"));
    assert!(!request_text.contains("secret-b"));
    assert_eq!(
        lines(&fixture.session_log)
            .iter()
            .filter(|line| line.as_str() == "app-server")
            .count(),
        1
    );

    sessions[0].shutdown(Duration::from_secs(5), false).unwrap();
    wait_for_rpc_count(&fixture, "thread/unsubscribe", 1);
    assert_eq!(sessions[1].thread_id(), Some("model-b"));
    assert_eq!(
        lines(&fixture.session_log)
            .iter()
            .filter(|line| line.as_str() == "app-server")
            .count(),
        1
    );
    sessions[1].shutdown(Duration::from_secs(5), false).unwrap();
}

#[test]
fn simultaneous_codex_sessions_join_one_host_start() {
    let _guard = CODEX_TEST_LOCK.lock();
    let fixture = FakeAgentFixture::new();
    let launches = Arc::new([
        fixture.launch(ProviderKind::Codex, "thread-simultaneous-a"),
        fixture.launch(ProviderKind::Codex, "thread-simultaneous-b"),
    ]);
    let barrier = Arc::new(Barrier::new(3));
    let mut workers = Vec::new();
    for index in 0..2 {
        let launches = Arc::clone(&launches);
        let barrier = Arc::clone(&barrier);
        workers.push(thread::spawn(move || {
            let (sender, receiver) = mpsc::channel();
            barrier.wait();
            let session = app_server::Session::spawn(
                &launches[index],
                launches.as_ref(),
                &AgentWorkspace::default(),
                move |value| {
                    let _ = sender.send(value);
                },
                |_| {},
            )
            .expect("simultaneous session should start");
            (session, receiver)
        }));
    }
    barrier.wait();

    let mut sessions: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().expect("session worker"))
        .collect();
    for (session, receiver) in &mut sessions {
        let _ = wait_for_codex_ready(session, receiver);
    }
    assert_eq!(
        fixture
            .rpc_methods()
            .iter()
            .filter(|method| method.as_str() == "initialize")
            .count(),
        1
    );
    assert_eq!(
        lines(&fixture.session_log)
            .iter()
            .filter(|line| line.as_str() == "app-server")
            .count(),
        1
    );

    for (session, _) in &mut sessions {
        session.shutdown(Duration::from_secs(5), false).unwrap();
    }
}

#[test]
fn simultaneous_codex_sessions_share_one_startup_failure() {
    let _guard = CODEX_TEST_LOCK.lock();
    let fixture = FakeAgentFixture::new();
    let mut raw_launches = [
        fixture.launch(ProviderKind::Codex, "thread-failed-start-a"),
        fixture.launch(ProviderKind::Codex, "thread-failed-start-b"),
    ];
    for launch in &mut raw_launches {
        launch
            .env
            .push(("NMT_FAKE_FAIL_INITIALIZE".into(), "1".into()));
    }
    let launches = Arc::new(raw_launches);
    let barrier = Arc::new(Barrier::new(3));
    let mut workers = Vec::new();
    for index in 0..2 {
        let launches = Arc::clone(&launches);
        let barrier = Arc::clone(&barrier);
        workers.push(thread::spawn(move || {
            barrier.wait();
            app_server::Session::spawn(
                &launches[index],
                launches.as_ref(),
                &AgentWorkspace::default(),
                |_| {},
                |_| {},
            )
            .err()
            .expect("startup should fail")
        }));
    }
    barrier.wait();

    let errors: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().expect("session worker"))
        .collect();
    assert!(
        errors
            .iter()
            .all(|error| error.contains("fake initialize failure"))
    );
    assert_eq!(
        lines(&fixture.session_log)
            .iter()
            .filter(|line| line.as_str() == "app-server")
            .count(),
        1
    );
}

#[test]
fn one_codex_host_rejects_incompatible_live_launch_settings() {
    let _guard = CODEX_TEST_LOCK.lock();
    let fixture = FakeAgentFixture::new();
    let first = fixture.launch(ProviderKind::Codex, "thread-compatible");
    let mut incompatible = fixture.launch(ProviderKind::Codex, "thread-incompatible");
    incompatible
        .env
        .push(("NMT_INCOMPATIBLE_SETTING".into(), "enabled".into()));
    let launches = [first.clone(), incompatible.clone()];

    let (sender, receiver) = mpsc::channel();
    let mut session = app_server::Session::spawn(
        &first,
        &launches,
        &AgentWorkspace::default(),
        move |value| {
            let _ = sender.send(value);
        },
        |_| {},
    )
    .expect("first session should start");
    assert_eq!(
        wait_for_codex_ready(&mut session, &receiver),
        "thread-compatible"
    );

    let error = app_server::Session::spawn(
        &incompatible,
        &launches,
        &AgentWorkspace::default(),
        |_| {},
        |_| {},
    )
    .err()
    .expect("incompatible launch should fail");
    assert!(error.contains("differ from the live shared host"));
    assert_eq!(session.thread_id(), Some("thread-compatible"));
    assert_eq!(
        lines(&fixture.session_log)
            .iter()
            .filter(|line| line.as_str() == "app-server")
            .count(),
        1
    );

    session.shutdown(Duration::from_secs(5), false).unwrap();
}

#[test]
fn two_codex_threads_recover_on_one_replacement_host() {
    let _guard = CODEX_TEST_LOCK.lock();
    let fixture = FakeAgentFixture::new();
    let ids = ["thread-crash-a", "thread-crash-b"];
    let mut launches = ids.map(|id| fixture.launch(ProviderKind::Codex, id));
    for launch in &mut launches {
        launch
            .env
            .push(("NMT_FAKE_EXIT_AFTER_THREAD_STARTS".into(), "2".into()));
    }

    let mut sessions = Vec::new();
    for launch in &launches {
        let (sender, receiver) = mpsc::channel();
        let mut session = app_server::Session::spawn(
            launch,
            &launches,
            &AgentWorkspace::default(),
            move |value| {
                let _ = sender.send(value);
            },
            |_| {},
        )
        .expect("session should start before the requested host exit");
        assert_eq!(
            wait_for_codex_ready(&mut session, &receiver),
            launch.model.as_deref().unwrap()
        );
        sessions.push((session, receiver));
    }

    for (session, receiver) in &mut sessions {
        wait_for_codex_host_exit(session, receiver);
    }
    assert_eq!(sessions[0].0.thread_id(), Some(ids[0]));
    assert_eq!(sessions[1].0.thread_id(), Some(ids[1]));

    let mut recovery_launches = launches.clone();
    for launch in &mut recovery_launches {
        launch
            .env
            .retain(|(name, _)| name != "NMT_FAKE_EXIT_AFTER_THREAD_STARTS");
    }
    let mut recovered = Vec::new();
    for (id, launch) in ids.into_iter().zip(&recovery_launches) {
        let (sender, receiver) = mpsc::channel();
        let mut session = app_server::Session::spawn_resuming(
            launch,
            &recovery_launches,
            &AgentWorkspace::default(),
            id.to_string(),
            true,
            move |value| {
                let _ = sender.send(value);
            },
            |_| {},
        )
        .expect("recovery session should attach");
        assert_eq!(wait_for_codex_ready(&mut session, &receiver), id);
        recovered.push(session);
    }

    assert_eq!(
        fixture
            .rpc_methods()
            .iter()
            .filter(|method| method.as_str() == "initialize")
            .count(),
        2
    );
    assert_eq!(
        fixture
            .rpc_methods()
            .iter()
            .filter(|method| method.as_str() == "thread/resume")
            .count(),
        2
    );
    assert_eq!(
        lines(&fixture.session_log)
            .iter()
            .filter(|line| line.as_str() == "app-server")
            .count(),
        2
    );

    drop(sessions);
    for session in &mut recovered {
        session.shutdown(Duration::from_secs(5), false).unwrap();
    }
}

#[test]
fn one_failed_codex_resume_does_not_block_another_session() {
    let _guard = CODEX_TEST_LOCK.lock();
    let fixture = FakeAgentFixture::new();
    let mut launches = [
        fixture.launch(ProviderKind::Codex, "thread-fails"),
        fixture.launch(ProviderKind::Codex, "thread-succeeds"),
    ];
    for launch in &mut launches {
        launch
            .env
            .push(("NMT_FAKE_FAIL_RESUME_ID".into(), "thread-fails".into()));
    }

    let (failed_sender, failed_receiver) = mpsc::channel();
    let mut failed = app_server::Session::spawn_resuming(
        &launches[0],
        &launches,
        &AgentWorkspace::default(),
        "thread-fails".into(),
        true,
        move |value| {
            let _ = failed_sender.send(value);
        },
        |_| {},
    )
    .expect("the host should start before the resume response");
    wait_for_codex_resume_failure(&mut failed, &failed_receiver);

    let (ready_sender, ready_receiver) = mpsc::channel();
    let mut ready = app_server::Session::spawn_resuming(
        &launches[1],
        &launches,
        &AgentWorkspace::default(),
        "thread-succeeds".into(),
        true,
        move |value| {
            let _ = ready_sender.send(value);
        },
        |_| {},
    )
    .expect("the second session should reuse the host");
    assert_eq!(
        wait_for_codex_ready(&mut ready, &ready_receiver),
        "thread-succeeds"
    );
    assert_eq!(
        lines(&fixture.session_log)
            .iter()
            .filter(|line| line.as_str() == "app-server")
            .count(),
        1
    );

    failed.shutdown(Duration::from_secs(5), false).unwrap();
    ready.shutdown(Duration::from_secs(5), false).unwrap();
}
