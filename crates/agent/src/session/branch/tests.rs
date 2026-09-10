use crate::chat::{ForkAnchor, ForkCheckpoint, ReplayTurn, SlashCommandOutcome};
use crate::claude_code::sessions::{ClaudeCheckpoint, ClaudeFork, FileRestoreAvailability};
use crate::session::branch::{
    BranchError, BranchReplay, BranchUpdate, BranchView, ConversationBranch, FailureStage,
    FileProgress, PromptTarget, RewindAction,
};
use crate::session::lifecycle::{SessionRuntime, StartOutcome, Status};
use crate::session::test_support::TestBackend;
use crate::session::{AgentKind, Backend, RecoveryIdentity};

fn runtime() -> SessionRuntime {
    let mut runtime = SessionRuntime::default();
    let mut backend = TestBackend::new([], SlashCommandOutcome::Accepted, Vec::new())
        .with_recovery(AgentKind::Claude, "source-session");
    backend.fork_accepted = true;
    let epoch = runtime.begin_start();
    assert!(matches!(
        runtime.install(epoch, Ok(Backend::Test(backend))),
        StartOutcome::Installed
    ));
    runtime.ready();
    runtime
}

fn backend(runtime: &mut SessionRuntime) -> &mut TestBackend {
    let Some(Backend::Test(backend)) = runtime.backend_mut() else {
        panic!("test backend required")
    };
    backend
}

fn checkpoint() -> ClaudeCheckpoint {
    ClaudeCheckpoint {
        user_message_id: "prompt-id".into(),
        parent_message_id: None,
        prompt: "continue here".into(),
        timestamp: None,
        file_restore_availability: FileRestoreAvailability::Available,
    }
}

fn fork_checkpoint() -> ForkCheckpoint {
    ForkCheckpoint {
        prompt: "continue here".into(),
        timestamp: None,
        anchor: ForkAnchor::CodexThrough("turn-id".into()),
    }
}

fn select(flow: &mut ConversationBranch, runtime: &SessionRuntime) {
    let request = flow
        .begin_rewind(runtime, Some("project".into()), None)
        .expect("read accepted");
    assert!(matches!(
        flow.checkpoints_loaded(runtime.epoch(), request, Ok(vec![checkpoint()])),
        BranchUpdate::Picker { unresolved: false }
    ));
    assert!(flow.select_checkpoint(runtime.epoch(), checkpoint()));
}

#[test]
fn mutually_exclusive_operations_hold_input_and_only_pickers_can_be_cancelled() {
    let mut runtime = runtime();
    let mut flow = ConversationBranch::default();
    select(&mut flow, &runtime);
    assert!(flow.holds_composer());
    assert!(flow.picker_is_open());
    assert!(!flow.is_working());
    assert!(matches!(
        flow.begin_fork(&mut runtime, None),
        Err(BranchError::Busy)
    ));
    assert!(flow.cancel_picker());
    assert!(!flow.holds_composer());

    select(&mut flow, &runtime);
    assert!(matches!(
        flow.rewind(&mut runtime, RewindAction::Files),
        BranchUpdate::RestoringFiles(RewindAction::Files)
    ));
    assert!(flow.is_working());
    assert!(!flow.cancel_picker());
    assert!(matches!(
        flow.files_completed(runtime.epoch(), Ok(())),
        BranchUpdate::FilesRestored
    ));
    assert!(!flow.holds_composer());
}

#[test]
fn cancelled_disk_reads_cannot_publish_into_a_new_picker() {
    let runtime = runtime();
    let mut flow = ConversationBranch::default();
    let old = flow.begin_rewind(&runtime, None, None).expect("old read");
    assert!(flow.cancel_picker());
    let new = flow.begin_rewind(&runtime, None, None).expect("new read");
    assert!(matches!(
        flow.checkpoints_loaded(runtime.epoch(), old, Ok(Vec::new())),
        BranchUpdate::Ignored
    ));
    assert!(matches!(flow.view(), BranchView::LoadingRewind));
    assert!(matches!(
        flow.checkpoints_loaded(runtime.epoch(), new, Ok(vec![checkpoint()])),
        BranchUpdate::Picker { .. }
    ));
}

#[test]
fn wrong_prompt_text_falls_back_to_picker_and_unknown_rows_cannot_be_selected() {
    let runtime = runtime();
    let mut flow = ConversationBranch::default();
    let request = flow
        .begin_rewind(
            &runtime,
            None,
            Some(PromptTarget {
                prompt: "different".into(),
                depth: 0,
            }),
        )
        .expect("read");
    assert!(matches!(
        flow.checkpoints_loaded(runtime.epoch(), request, Ok(vec![checkpoint()])),
        BranchUpdate::Picker { unresolved: true }
    ));
    let mut other = checkpoint();
    other.user_message_id = "not-listed".into();
    assert!(!flow.select_checkpoint(runtime.epoch(), other));
    assert!(matches!(flow.view(), BranchView::RewindCheckpoints(_)));
}

#[test]
fn combined_file_failure_does_not_create_a_conversation_and_can_retry_the_files() {
    let mut runtime = runtime();
    let mut flow = ConversationBranch::default();
    select(&mut flow, &runtime);
    assert!(matches!(
        flow.rewind(&mut runtime, RewindAction::FilesAndConversation),
        BranchUpdate::RestoringFiles(_)
    ));
    let BranchUpdate::Failed(failure) =
        flow.files_completed(runtime.epoch(), Err("expired".into()))
    else {
        panic!("file failure expected")
    };
    assert_eq!(failure.stage, FailureStage::Files);
    assert_eq!(failure.files, FileProgress::NotConfirmed);
    assert!(flow.picker_is_open());
    assert!(matches!(
        flow.rewind(&mut runtime, RewindAction::FilesAndConversation),
        BranchUpdate::RestoringFiles(_)
    ));
    assert_eq!(backend(&mut runtime).file_restore_requests.len(), 2);
}

#[test]
fn retry_after_file_success_only_retries_conversation_and_moves_replay_once() {
    let mut runtime = runtime();
    let mut flow = ConversationBranch::default();
    select(&mut flow, &runtime);
    flow.rewind(&mut runtime, RewindAction::FilesAndConversation);
    let BranchUpdate::CreateFork(request) = flow.files_completed(runtime.epoch(), Ok(())) else {
        panic!("fork expected")
    };
    let BranchUpdate::Failed(failure) =
        flow.fork_created(runtime.epoch(), request, Err("disk full".into()))
    else {
        panic!("fork failure expected")
    };
    assert_eq!(failure.stage, FailureStage::Conversation);
    assert_eq!(failure.files, FileProgress::Restored);

    let BranchUpdate::CreateFork(retry) =
        flow.rewind(&mut runtime, RewindAction::FilesAndConversation)
    else {
        panic!("retry only the fork")
    };
    assert_eq!(backend(&mut runtime).file_restore_requests.len(), 1);
    let replay = vec![ReplayTurn::default()];
    let allocation = replay.as_ptr();
    let BranchUpdate::StartSession(identity) = flow.fork_created(
        runtime.epoch(),
        retry,
        Ok(ClaudeFork {
            session_id: Some("copy".into()),
            replay,
        }),
    ) else {
        panic!("restart expected")
    };
    assert!(flow.ready(runtime.epoch()).is_none());
    let epoch = runtime.begin_start();
    assert!(flow.starting(epoch, identity.as_ref()));
    assert!(flow.ready(epoch - 1).is_none());
    let completion = flow.ready(epoch).expect("ready publishes the copy");
    assert_eq!(completion.files, FileProgress::Restored);
    assert_eq!(completion.prompt, "continue here");
    let replay = completion.replay.expect("local replay");
    assert_eq!(replay.as_ptr(), allocation);
    assert!(flow.ready(epoch).is_none());
    assert!(!flow.holds_composer());
}

#[test]
fn conversation_only_can_start_before_the_first_prompt_without_touching_files() {
    let mut runtime = runtime();
    let mut flow = ConversationBranch::default();
    select(&mut flow, &runtime);
    let BranchUpdate::CreateFork(request) = flow.rewind(&mut runtime, RewindAction::Conversation)
    else {
        panic!("fork expected")
    };
    assert!(backend(&mut runtime).file_restore_requests.is_empty());
    assert!(matches!(
        flow.fork_created(
            runtime.epoch(),
            request,
            Ok(ClaudeFork {
                session_id: None,
                replay: Vec::new()
            })
        ),
        BranchUpdate::StartSession(None)
    ));
    let epoch = runtime.begin_start();
    assert!(flow.starting(epoch, None));
    let completion = flow
        .ready(epoch)
        .expect("empty branch is still a completed operation");
    assert!(completion.replay.expect("local replay").is_empty());
}

#[test]
fn startup_failure_reports_confirmed_file_changes_and_drops_unpublished_replay() {
    let mut runtime = runtime();
    let mut flow = ConversationBranch::default();
    select(&mut flow, &runtime);
    flow.rewind(&mut runtime, RewindAction::FilesAndConversation);
    let BranchUpdate::CreateFork(request) = flow.files_completed(runtime.epoch(), Ok(())) else {
        panic!("fork expected")
    };
    let BranchUpdate::StartSession(identity) = flow.fork_created(
        runtime.epoch(),
        request,
        Ok(ClaudeFork {
            session_id: Some("copy".into()),
            replay: vec![ReplayTurn::default()],
        }),
    ) else {
        panic!("restart expected")
    };
    let epoch = runtime.begin_start();
    flow.starting(epoch, identity.as_ref());
    assert!(matches!(
        runtime.install(epoch, Err("cannot spawn".into())),
        StartOutcome::Failed(_)
    ));
    let failure = flow
        .failed(&mut runtime, "cannot spawn".into())
        .expect("failure");
    assert_eq!(failure.stage, FailureStage::Startup);
    assert_eq!(failure.files, FileProgress::Restored);
    assert_eq!(runtime.status(), Status::Exited);
    assert!(flow.ready(epoch).is_none());
}

#[test]
fn unavailable_file_snapshots_are_rejected_without_a_provider_request() {
    let mut runtime = runtime();
    let mut flow = ConversationBranch::default();
    let mut checkpoint = checkpoint();
    checkpoint.file_restore_availability = FileRestoreAvailability::Unavailable;
    let request = flow.begin_rewind(&runtime, None, None).expect("read");
    flow.checkpoints_loaded(runtime.epoch(), request, Ok(vec![checkpoint.clone()]));
    flow.select_checkpoint(runtime.epoch(), checkpoint);
    let BranchUpdate::Failed(failure) = flow.rewind(&mut runtime, RewindAction::Files) else {
        panic!("unavailable files")
    };
    assert_eq!(failure.error, BranchError::FilesUnavailable);
    assert!(backend(&mut runtime).file_restore_requests.is_empty());
    assert!(flow.picker_is_open());
}

#[test]
fn abandoned_file_requests_are_drained_before_another_restore_can_start() {
    let mut runtime = runtime();
    let mut flow = ConversationBranch::default();
    select(&mut flow, &runtime);
    flow.rewind(&mut runtime, RewindAction::Files);
    flow.clear();
    select(&mut flow, &runtime);
    let BranchUpdate::Failed(failure) = flow.rewind(&mut runtime, RewindAction::Files) else {
        panic!("old request still outstanding")
    };
    assert_eq!(failure.error, BranchError::Busy);
    assert!(matches!(
        flow.files_completed(runtime.epoch(), Ok(())),
        BranchUpdate::Ignored
    ));
    assert!(matches!(
        flow.rewind(&mut runtime, RewindAction::Files),
        BranchUpdate::RestoringFiles(_)
    ));
    assert_eq!(backend(&mut runtime).file_restore_requests.len(), 2);
}

#[test]
fn cancelled_protocol_list_is_drained_and_cannot_select_a_new_branch() {
    let mut runtime = runtime();
    let mut flow = ConversationBranch::default();
    flow.begin_fork(&mut runtime, None).expect("list accepted");
    assert!(flow.cancel_picker());
    assert!(matches!(
        flow.begin_fork(&mut runtime, None),
        Err(BranchError::Busy)
    ));
    assert!(matches!(
        flow.fork_checkpoints(&mut runtime, Ok(vec![fork_checkpoint()])),
        BranchUpdate::Ignored
    ));
    assert!(backend(&mut runtime).fork_requests.is_empty());
    flow.begin_fork(&mut runtime, None)
        .expect("new list accepted after drain");
    assert!(matches!(
        flow.fork_checkpoints(&mut runtime, Ok(vec![fork_checkpoint()])),
        BranchUpdate::Picker { .. }
    ));
}

#[test]
fn protocol_branch_returns_the_prompt_only_with_its_own_replay() {
    let mut runtime = runtime();
    let mut flow = ConversationBranch::default();
    flow.begin_fork(
        &mut runtime,
        Some(PromptTarget {
            prompt: "continue here".into(),
            depth: 0,
        }),
    )
    .expect("list accepted");
    assert!(matches!(
        flow.fork_checkpoints(&mut runtime, Ok(vec![fork_checkpoint()])),
        BranchUpdate::Branching
    ));
    assert_eq!(
        backend(&mut runtime).fork_requests,
        [fork_checkpoint().anchor]
    );
    assert_eq!(runtime.status(), Status::Starting);
    assert!(!flow.cancel_picker());
    assert!(matches!(
        flow.replayed(runtime.epoch() + 1),
        BranchReplay::Ignore
    ));
    let BranchReplay::Complete(completion) = flow.replayed(runtime.epoch()) else {
        panic!("matching replay")
    };
    assert_eq!(completion.prompt, "continue here");
    assert!(completion.replay.is_none());
    assert!(matches!(
        flow.replayed(runtime.epoch()),
        BranchReplay::Unrelated
    ));
}

#[test]
fn rejected_protocol_branch_preserves_picker_and_reported_failure_restores_status() {
    let mut runtime = runtime();
    let mut flow = ConversationBranch::default();
    flow.begin_fork(&mut runtime, None).expect("list");
    flow.fork_checkpoints(&mut runtime, Ok(vec![fork_checkpoint()]));
    backend(&mut runtime).fork_accepted = false;
    assert!(matches!(
        flow.fork(&mut runtime, fork_checkpoint()),
        BranchUpdate::Failed(_)
    ));
    assert!(flow.picker_is_open());
    assert_eq!(runtime.status(), Status::Idle);
    backend(&mut runtime).fork_accepted = true;
    flow.fork(&mut runtime, fork_checkpoint());
    let failure = flow
        .failed(&mut runtime, "provider rejected branch".into())
        .expect("failure");
    assert_eq!(failure.stage, FailureStage::ProtocolFork);
    assert_eq!(runtime.status(), Status::Idle);
    assert!(!flow.holds_composer());
}

#[test]
fn replacement_epochs_reject_disk_results_and_unrelated_starts_drop_prepared_history() {
    let mut runtime = runtime();
    let mut flow = ConversationBranch::default();
    let request = flow.begin_rewind(&runtime, None, None).expect("read");
    let epoch = runtime.begin_start();
    flow.starting(epoch, None);
    runtime.ready();
    assert!(matches!(
        flow.checkpoints_loaded(epoch, request, Ok(vec![checkpoint()])),
        BranchUpdate::Ignored
    ));
    select(&mut flow, &runtime);
    let BranchUpdate::CreateFork(request) = flow.rewind(&mut runtime, RewindAction::Conversation)
    else {
        panic!("fork")
    };
    flow.fork_created(
        epoch,
        request,
        Ok(ClaudeFork {
            session_id: Some("copy".into()),
            replay: vec![ReplayTurn::default()],
        }),
    );
    let epoch = runtime.begin_start();
    assert!(!flow.starting(
        epoch,
        Some(&RecoveryIdentity::new(AgentKind::Claude, "unrelated"))
    ));
    assert!(flow.ready(epoch).is_none());
}
