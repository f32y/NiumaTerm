use std::sync::Arc;
use std::time::SystemTime;

use crate::chat::{
    Item, SessionSummary, SlashCommandOutcome, SlashCommandRunPolicy, ThreadSettings,
};
use crate::session::commands::{CommandAdmission, CommandQueue, PendingSlashCommand};
use crate::session::delivery::MessageDelivery;
use crate::session::history::{CountPublication, SessionHistory};
use crate::session::lifecycle::{SessionRuntime, Status};
use crate::session::naming::ConversationNaming;
use crate::session::restore::SettingsSeed;
use crate::session::settings::{ConversationSettings, RememberedSettings};
use crate::session::test_support::TestBackend;
use crate::session::update_readiness::{ConversationWork, Readiness};
use crate::session::workflows::WorkflowData;
use crate::session::{AgentKind, Backend, RenameOutcome};
use crate::workflow::{
    WorkflowRefreshRequest, WorkflowRefreshResult, WorkflowRun, WorkflowSource,
    WorkflowTranscriptRead,
};

#[test]
fn queued_commands_wait_for_real_turn_and_exit_discards_pending_work() {
    let mut commands = CommandQueue::default();

    let command = PendingSlashCommand::new("compact", String::new());

    assert!(matches!(
        commands.while_busy(command, SlashCommandRunPolicy::QueueUntilIdle),
        CommandAdmission::Queued { count: 1, .. }
    ));

    let command = commands.queue.pop_front().unwrap();

    let mut backend = Backend::Test(TestBackend::new(
        [],
        SlashCommandOutcome::Accepted,
        Vec::new(),
    ));

    assert!(matches!(
        commands.execute(Some(&mut backend), &command),
        SlashCommandOutcome::Accepted
    ));
    assert!(!commands.settle(
        &SlashCommandOutcome::Completed { message: None },
        Status::Running
    ));
    assert!(commands.turn_started());
    assert!(!commands.turn_started());

    commands.while_busy(command, SlashCommandRunPolicy::QueueUntilIdle);

    assert!(commands.clear());
    assert!(commands.queue.is_empty());
    assert!(!commands.awaiting_turn);
}

#[test]
fn ready_priority_keeps_profile_then_current_then_branch_settings() {
    let mut settings = ConversationSettings {
        seed: SettingsSeed::Defaults,
        ..Default::default()
    };

    let stored = ThreadSettings {
        model: Some("remembered".into()),
        effort: Some("high".into()),
        ..Default::default()
    };

    settings.ready(
        AgentKind::Claude,
        ThreadSettings::default(),
        Some(&stored),
        Some("profile"),
        None,
    );

    assert_eq!(settings.settings.model.as_deref(), Some("profile"));
    assert_eq!(settings.seed, SettingsSeed::None);

    settings.ready(
        AgentKind::Claude,
        ThreadSettings {
            model: Some("reported".into()),
            ..Default::default()
        },
        None,
        None,
        None,
    );

    assert_eq!(settings.settings.model.as_deref(), Some("profile"));
    assert_eq!(settings.settings.effort.as_deref(), Some("high"));

    settings.restore_on_ready = Some(ThreadSettings {
        model: Some("branch".into()),
        ..Default::default()
    });

    settings.ready(
        AgentKind::Claude,
        ThreadSettings::default(),
        None,
        None,
        None,
    );

    assert_eq!(settings.settings.model.as_deref(), Some("branch"));
    assert!(settings.restore_on_ready.is_none());
}

#[test]
fn remembered_profiles_fall_back_to_legacy_provider_without_crossing_named_profiles() {
    let mut settings = RememberedSettings::default();

    settings.remember(
        AgentKind::Codex,
        "",
        ThreadSettings {
            model: Some("legacy".into()),
            ..Default::default()
        },
    );

    settings.remember(
        AgentKind::Codex,
        "work",
        ThreadSettings {
            model: Some("work-model".into()),
            ..Default::default()
        },
    );

    assert_eq!(
        settings
            .get(AgentKind::Codex, "work")
            .unwrap()
            .model
            .as_deref(),
        Some("work-model")
    );
    assert_eq!(
        settings
            .get(AgentKind::Codex, "personal")
            .unwrap()
            .model
            .as_deref(),
        Some("legacy")
    );
    assert!(settings.get(AgentKind::Claude, "personal").is_none());
}

fn summary(id: &str) -> SessionSummary {
    SessionSummary {
        id: id.into(),
        title: id.into(),
        branch: None,
        cwd: None,
        last_active: SystemTime::UNIX_EPOCH,
        snippet: None,
    }
}

#[test]
fn search_retires_disk_reads_and_next_history_page_replaces_matches() {
    let mut history = SessionHistory::default();

    let request = history.begin_filesystem_history(None, 1);

    assert!(history.search_results(vec![summary("match")]));
    assert!(matches!(
        history.publish_filesystem_count(&request, None, 1, 8),
        CountPublication::Stale
    ));
    assert!(!history.publish_filesystem_rows(&request, None, 1, vec![summary("old")]));

    history.append_page(vec![summary("recent"), summary("recent")]);

    assert_eq!(history.sessions, vec![summary("recent")]);
    assert!(!history.showing_search);
    assert!(!history.search_results(Vec::new()));
    assert_eq!(history.sessions, vec![summary("recent")]);
}

#[test]
fn workflow_readers_keep_separate_content_and_acknowledge_source_revisions() {
    let mut workflows = WorkflowData::default();

    assert!(workflows.claim_restore("session"));
    assert!(!workflows.claim_restore("session"));

    let first = workflows.open_agent("run", "first");
    let _second = workflows.open_agent("run", "second");

    assert!(workflows.apply_transcript("run", "first", vec![Item::Error { text: "old".into() }]));
    assert!(workflows.apply_transcript("run", "second", vec![Item::Error { text: "new".into() }]));

    workflows.accept_revision(&WorkflowRefreshResult {
        task_id: "run".into(),
        transcript: Some(WorkflowTranscriptRead {
            agent_id: "second".into(),
            items: Vec::new(),
            revision: 128,
        }),
        ..Default::default()
    });

    drop(first);

    let requests = workflows.scope_requests(vec![WorkflowRefreshRequest {
        task_id: "run".into(),
        agent_ids: vec![],
        open_agent: None,
        transcript_revision: None,
    }]);

    assert_eq!(requests[0].open_agent.as_deref(), Some("second"));
    assert_eq!(requests[0].transcript_revision, Some(128));

    for (agent_id, expected) in [("first", "old"), ("second", "new")] {
        let conversation = workflows
            .conversation("run", agent_id)
            .unwrap()
            .conversation
            .borrow();

        assert_eq!(conversation.content.entries().len(), 1);
        assert_eq!(
            conversation.content.entries()[0].item,
            Item::Error {
                text: expected.into()
            }
        );
    }

    workflows.clear();

    assert!(workflows.conversation("run", "second").is_none());
    assert!(workflows.claim_restore("session"));
}

#[test]
fn workflow_refresh_uses_the_supplied_source_and_keeps_its_session_epoch() {
    struct MemorySource;

    impl WorkflowSource for MemorySource {
        fn restore(&self, _: Option<&str>, _: &str) -> Result<Vec<WorkflowRun>, String> {
            Ok(Vec::new())
        }

        fn refresh(
            &self,
            cwd: Option<&str>,
            session_id: &str,
            request: &WorkflowRefreshRequest,
        ) -> WorkflowRefreshResult {
            assert_eq!(cwd, Some("workspace"));
            assert_eq!(session_id, "session");
            assert_eq!(request.open_agent.as_deref(), Some("member"));

            WorkflowRefreshResult {
                task_id: request.task_id.clone(),
                transcript: Some(WorkflowTranscriptRead {
                    agent_id: "member".into(),
                    items: vec![Item::Error {
                        text: "source response".into(),
                    }],
                    revision: 7,
                }),
                ..Default::default()
            }
        }
    }

    let mut backend = TestBackend::new([], SlashCommandOutcome::NotReady, vec![])
        .with_recovery(AgentKind::DeepSeek, "session");

    backend.workflow_source = Some(Arc::new(MemorySource));

    backend.workflow_requests = vec![WorkflowRefreshRequest {
        task_id: "run".into(),
        ..Default::default()
    }];

    let mut runtime = SessionRuntime::default();

    runtime.install(runtime.epoch(), Ok(Backend::Test(backend)));

    let mut workflows = WorkflowData::default();

    let _reader = workflows.open_agent("run", "member");

    let plan = workflows
        .refresh_plan(&runtime, Some("workspace".into()))
        .unwrap();

    let epoch = plan.epoch;

    let mut results = plan.read();

    let result = results.remove(0);

    assert!(runtime.is_current(epoch));

    workflows.accept_revision(&result);

    let transcript = result.transcript.unwrap();

    assert!(workflows.apply_transcript(&result.task_id, &transcript.agent_id, transcript.items));

    let requests = workflows.scope_requests(vec![WorkflowRefreshRequest {
        task_id: "run".into(),
        ..Default::default()
    }]);

    assert_eq!(requests[0].transcript_revision, Some(7));

    runtime.suspend_for_update();

    assert!(!runtime.is_current(epoch));
}

#[test]
fn rename_waits_for_identity_and_retries_rejection_without_losing_latest_title() {
    let mut naming = ConversationNaming::default();

    naming.rename("first");

    naming.rename("latest");

    naming.sync(None);

    assert_eq!(naming.pending.as_deref(), Some("latest"));

    let mut session = TestBackend::new([], SlashCommandOutcome::NotReady, vec![])
        .with_recovery(AgentKind::Codex, "thread");

    session.rename_outcome = RenameOutcome::Rejected;

    let mut backend = Backend::Test(session);

    naming.sync(Some(&mut backend));

    assert_eq!(naming.pending.as_deref(), Some("latest"));

    if let Backend::Test(session) = &mut backend {
        session.rename_outcome = RenameOutcome::Accepted;
    }

    naming.sync(Some(&mut backend));

    assert!(naming.pending.is_none());
    assert!(naming.named);
}

#[test]
fn update_requires_idle_work_and_identity_only_for_nonempty_conversations() {
    let mut runtime = SessionRuntime::default();

    let commands = CommandQueue::default();
    let delivery = MessageDelivery::new(AgentKind::Codex);

    let mut work = ConversationWork {
        approval_open: false,
        branch_pending: false,
        compacting: false,
        empty: true,
    };

    assert!(matches!(
        work.readiness(&runtime, &commands, &delivery),
        Readiness::ActiveWork
    ));

    runtime.ready();

    assert!(matches!(
        work.readiness(&runtime, &commands, &delivery),
        Readiness::Ready(None)
    ));

    work.empty = false;

    assert!(matches!(
        work.readiness(&runtime, &commands, &delivery),
        Readiness::MissingIdentity
    ));

    work.approval_open = true;

    assert!(matches!(
        work.readiness(&runtime, &commands, &delivery),
        Readiness::ActiveWork
    ));
}
