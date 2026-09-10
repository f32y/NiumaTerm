use std::time::SystemTime;

use crate::chat::{ReplayTurn, SessionSummary, SlashCommandOutcome};
use crate::session::lifecycle::{SessionRuntime, StartOutcome, Status};
use crate::session::restore::{
    ConversationRestore, ReadyAction, ReplayAction, ReplayLoaded, ReplayRead, ResumeStart,
    SettingsSeed,
};
use crate::session::test_support::TestBackend;
use crate::session::{AgentKind, Backend, RecoveryIdentity};

fn summary(id: &str) -> SessionSummary {
    SessionSummary {
        id: id.into(),
        title: id.into(),
        branch: None,
        cwd: Some("project".into()),
        last_active: SystemTime::UNIX_EPOCH,
        snippet: None,
    }
}

fn read(restore: &mut ConversationRestore, runtime: &mut SessionRuntime) -> ReplayRead {
    match restore.begin(
        runtime,
        AgentKind::Claude,
        &summary("selected"),
        Some("project"),
    ) {
        ResumeStart::ReadReplay(request) => request,
        _ => panic!("Claude must load local history"),
    }
}

fn ready_runtime() -> SessionRuntime {
    let mut runtime = SessionRuntime::default();
    runtime.ready();
    runtime
}

#[test]
fn another_directory_routes_without_changing_the_current_session() {
    let mut runtime = ready_runtime();
    let mut restore = ConversationRestore::default();

    for kind in AgentKind::ALL {
        assert!(matches!(
            restore.begin(&mut runtime, kind, &summary("selected"), Some("other")),
            ResumeStart::Elsewhere { cwd, session_id } if cwd == "project" && session_id == "selected"
        ));
        assert_eq!(runtime.status(), Status::Idle);
        assert_eq!(runtime.epoch(), 0);
    }

    assert!(matches!(
        restore.begin(&mut runtime, AgentKind::Claude, &summary("selected"), None),
        ResumeStart::ReadReplay(_)
    ));
}

#[test]
fn rejected_protocol_resume_preserves_status_and_accepts_a_later_attempt() {
    let mut runtime = ready_runtime();
    let mut restore = ConversationRestore::default();
    runtime.turn_started();

    for kind in [AgentKind::Codex, AgentKind::DeepSeek] {
        assert!(matches!(
            restore.begin(&mut runtime, kind, &summary("selected"), Some("project")),
            ResumeStart::Rejected
        ));
        assert_eq!(runtime.status(), Status::Running);
    }

    assert!(matches!(
        restore.begin(
            &mut runtime,
            AgentKind::Claude,
            &summary("selected"),
            Some("project")
        ),
        ResumeStart::ReadReplay(_)
    ));
}

#[test]
fn accepted_protocol_resume_is_busy_until_replay_and_failure_restores_old_status() {
    for kind in [AgentKind::Codex, AgentKind::DeepSeek] {
        let mut runtime = SessionRuntime::default();
        let mut restore = ConversationRestore::default();
        let mut backend = TestBackend::new([], SlashCommandOutcome::NotReady, Vec::new());
        backend.resume_accepted = true;
        let epoch = runtime.begin_start();
        assert!(matches!(
            runtime.install(epoch, Ok(Backend::Test(backend))),
            StartOutcome::Installed
        ));
        runtime.turn_started();

        assert!(matches!(
            restore.begin(&mut runtime, kind, &summary("selected"), Some("project")),
            ResumeStart::Requested
        ));
        assert!(matches!(
            restore.begin(&mut runtime, kind, &summary("other"), Some("project")),
            ResumeStart::Busy
        ));
        assert_eq!(restore.replayed(epoch + 1), ReplayAction::Ignore);
        assert!(restore.failed(&mut runtime));
        assert_eq!(runtime.status(), Status::Running);

        assert!(matches!(
            restore.begin(&mut runtime, kind, &summary("selected"), Some("project")),
            ResumeStart::Requested
        ));
        assert_eq!(restore.replayed(epoch), ReplayAction::Replace);
        assert_eq!(restore.replayed(epoch), ReplayAction::Append);
    }
}

#[test]
fn an_old_read_cannot_replace_a_new_selection_even_for_the_same_session_id() {
    let mut runtime = ready_runtime();
    let mut restore = ConversationRestore::default();
    let old = read(&mut restore, &mut runtime);
    restore.failed(&mut runtime);
    let current = read(&mut restore, &mut runtime);

    assert!(matches!(
        restore.loaded(&mut runtime, old, Some("project"), Ok(Vec::new())),
        ReplayLoaded::Stale
    ));
    assert!(
        matches!(restore.loaded(&mut runtime, current, Some("project"), Ok(Vec::new())), ReplayLoaded::Restart(identity) if identity.id == "selected")
    );
}

#[test]
fn a_read_from_an_old_epoch_cannot_restart_or_change_the_new_status() {
    let mut runtime = ready_runtime();
    let mut restore = ConversationRestore::default();
    let request = read(&mut restore, &mut runtime);
    runtime.begin_start();
    runtime.turn_started();

    assert!(matches!(
        restore.loaded(&mut runtime, request, Some("project"), Ok(Vec::new())),
        ReplayLoaded::Stale
    ));
    assert_eq!(runtime.status(), Status::Running);
}

#[test]
fn changed_directory_retires_the_read_and_releases_the_old_status() {
    for cwd in [None, Some("other")] {
        let mut runtime = ready_runtime();
        let mut restore = ConversationRestore::default();
        let request = read(&mut restore, &mut runtime);

        assert!(matches!(
            restore.loaded(&mut runtime, request, cwd, Ok(Vec::new())),
            ReplayLoaded::Cancelled
        ));
        assert_eq!(runtime.status(), Status::Idle);
    }
}

#[test]
fn failed_disk_read_does_not_start_a_replacement() {
    let mut runtime = ready_runtime();
    let mut restore = ConversationRestore::default();
    let request = read(&mut restore, &mut runtime);

    assert!(
        matches!(restore.loaded(&mut runtime, request, Some("project"), Err("unreadable".into())), ReplayLoaded::Failed(message) if message == "unreadable")
    );
    assert_eq!(runtime.epoch(), 0);
    assert_eq!(runtime.status(), Status::Idle);
    assert!(!matches!(restore.ready(0), ReadyAction::Replay(_)));
    read(&mut restore, &mut runtime);
}

#[test]
fn local_replay_moves_once_after_the_matching_restart_becomes_ready() {
    let mut runtime = ready_runtime();
    let mut restore = ConversationRestore::default();
    let request = read(&mut restore, &mut runtime);
    let replay = vec![ReplayTurn {
        seconds: Some(12),
        ..ReplayTurn::default()
    }];
    let buffer = replay.as_ptr();

    let ReplayLoaded::Restart(identity) =
        restore.loaded(&mut runtime, request, Some("project"), Ok(replay))
    else {
        panic!("read must prepare a restart");
    };
    assert!(!matches!(
        restore.ready(runtime.epoch()),
        ReadyAction::Replay(_)
    ));

    let epoch = runtime.begin_start();
    restore.starting(epoch, Some(&identity));
    assert!(!matches!(restore.ready(epoch - 1), ReadyAction::Replay(_)));
    let ReadyAction::Replay(replay) = restore.ready(epoch) else {
        panic!("matching ready must publish history");
    };
    assert_eq!(
        replay.as_ptr(),
        buffer,
        "the replay allocation is transferred, not copied"
    );
    assert_eq!(replay[0].seconds, Some(12));
    assert!(!matches!(restore.ready(epoch), ReadyAction::Replay(_)));
}

#[test]
fn unrelated_starts_and_startup_failures_drop_unpublished_replay() {
    for recovery in [
        None,
        Some(RecoveryIdentity::new(AgentKind::Claude, "other")),
        Some(RecoveryIdentity::new(AgentKind::Codex, "selected")),
    ] {
        let mut runtime = ready_runtime();
        let mut restore = ConversationRestore::default();
        let request = read(&mut restore, &mut runtime);
        restore.loaded(
            &mut runtime,
            request,
            Some("project"),
            Ok(vec![ReplayTurn::default()]),
        );
        let epoch = runtime.begin_start();
        restore.starting(epoch, recovery.as_ref());
        assert!(!matches!(restore.ready(epoch), ReadyAction::Replay(_)));
    }

    let mut runtime = ready_runtime();
    let mut restore = ConversationRestore::default();
    let request = read(&mut restore, &mut runtime);
    let ReplayLoaded::Restart(identity) =
        restore.loaded(&mut runtime, request, Some("project"), Ok(Vec::new()))
    else {
        panic!("read must prepare a restart");
    };
    let epoch = runtime.begin_start();
    restore.starting(epoch, Some(&identity));
    assert!(matches!(
        runtime.install(epoch, Err("cannot spawn".into())),
        StartOutcome::Failed(_)
    ));
    assert!(restore.failed(&mut runtime));
    assert_eq!(runtime.status(), Status::Exited);
    assert_eq!(runtime.start_failure(), Some("cannot spawn"));
    assert!(!matches!(restore.ready(epoch), ReadyAction::Replay(_)));
}

#[test]
fn resumed_settings_seed_only_values_missing_from_the_provider() {
    assert_eq!(
        SettingsSeed::resumed(AgentKind::Codex),
        SettingsSeed::Reviewer
    );
    assert_eq!(
        SettingsSeed::resumed(AgentKind::Claude),
        SettingsSeed::Defaults
    );
    assert_eq!(
        SettingsSeed::resumed(AgentKind::DeepSeek),
        SettingsSeed::Defaults
    );
}

#[test]
fn missing_history_is_an_error_instead_of_an_empty_replay() {
    let mut runtime = ready_runtime();
    let mut restore = ConversationRestore::default();
    let mut selected = summary("missing");
    selected.id = uuid::Uuid::new_v4().to_string();
    let ResumeStart::ReadReplay(request) =
        restore.begin(&mut runtime, AgentKind::Claude, &selected, Some("project"))
    else {
        panic!("Claude must load history");
    };

    assert!(request.load().is_err());
}

#[test]
fn old_backend_ready_and_replay_cannot_complete_an_in_progress_disk_read() {
    let mut runtime = ready_runtime();
    let mut restore = ConversationRestore::default();
    let request = read(&mut restore, &mut runtime);

    assert!(matches!(
        restore.ready(runtime.epoch()),
        ReadyAction::Ignore
    ));
    assert_eq!(restore.replayed(runtime.epoch()), ReplayAction::Ignore);
    assert!(matches!(
        restore.loaded(&mut runtime, request, Some("project"), Ok(Vec::new())),
        ReplayLoaded::Restart(_)
    ));
}
