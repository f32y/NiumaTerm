use std::time::{Duration, Instant};

use crate::chat::{
    Question, QuestionInput, QuestionMode, QuestionOption, QuestionRequest, QuestionResolution,
    SlashCommandOutcome, ThreadSettings,
};
use crate::session::input::{
    ApprovalOutcome, QuestionAction, QuestionDraft, QuestionError, QuestionKey, QuestionStatus,
    SessionInput, Submission,
};
use crate::session::lifecycle::{SessionRuntime, StartOutcome};
use crate::session::test_support::{InputResponse, TestBackend};
use crate::session::{AgentKind, Backend, RecoveryIdentity};

fn runtime(kind: AgentKind, id: &str) -> SessionRuntime {
    let mut runtime = SessionRuntime::default();

    let backend =
        TestBackend::new([], SlashCommandOutcome::Accepted, Vec::new()).with_recovery(kind, id);

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
        panic!("test backend required");
    };

    backend
}

fn question(text: &str, multi_select: bool, labels: &[&str]) -> Question {
    Question {
        input: QuestionInput::Text,
        header: None,
        question: text.to_owned(),
        multi_select,
        options: labels
            .iter()
            .map(|label| QuestionOption {
                label: (*label).to_owned(),
                description: None,
            })
            .collect(),
    }
}

fn request(id: &str, mode: QuestionMode) -> QuestionRequest {
    QuestionRequest {
        id: id.into(),
        mode,
        questions: vec![question("Choose", false, &["one", "two"])],
    }
}

fn receive(
    input: &mut SessionInput,
    runtime: &SessionRuntime,
    id: &str,
    mode: QuestionMode,
) -> QuestionKey {
    let index = input
        .receive(runtime, request(id, mode))
        .expect("new request");

    input.batches()[index].key()
}

fn submit(
    input: &mut SessionInput,
    runtime: &mut SessionRuntime,
    key: QuestionKey,
    action: QuestionAction,
) -> Submission {
    input.submit(
        runtime,
        key,
        action,
        &ThreadSettings::default(),
        Instant::now(),
    )
}

#[test]
fn single_select_replaces_and_multi_select_answers_follow_option_order() {
    let mut draft = QuestionDraft::new(vec![
        question("Database", false, &["Postgres", "SQLite"]),
        question("Extras", true, &["Metrics", "Tracing", "Audit"]),
    ]);

    assert!(!draft.is_complete());

    draft.toggle(0, 1);
    draft.toggle(0, 0);
    draft.toggle(1, 2);
    draft.toggle(1, 0);

    assert!(draft.is_complete());
    assert_eq!(
        draft.answers(),
        vec![vec!["Postgres"], vec!["Metrics", "Audit"]]
    );

    draft.toggle(1, 0);
    draft.toggle(1, 2);

    assert!(!draft.is_complete());
}

#[test]
fn submission_is_exclusive_and_failure_keeps_the_edited_draft_for_retry() {
    let mut runtime = runtime(AgentKind::Codex, "thread");
    let mut input = SessionInput::default();

    input.restore(&mut runtime);

    let first = receive(&mut input, &runtime, "first", QuestionMode::Async);
    let second = receive(&mut input, &runtime, "second", QuestionMode::Async);

    input
        .draft_mut(first)
        .unwrap()
        .set_text(0, "  edited answer  ".into());

    assert_eq!(
        submit(&mut input, &mut runtime, first, QuestionAction::Answer),
        Submission::Waiting
    );
    assert_eq!(
        submit(&mut input, &mut runtime, first, QuestionAction::Answer),
        Submission::Ignored
    );
    assert_eq!(
        submit(&mut input, &mut runtime, second, QuestionAction::Answer),
        Submission::Ignored
    );
    assert_eq!(backend(&mut runtime).input_responses.len(), 1);
    assert!(input.submission_failed(runtime.epoch(), "first", "retry".into()));
    assert_eq!(input.batches()[0].text(0), "  edited answer  ");
    assert_eq!(
        submit(&mut input, &mut runtime, first, QuestionAction::Answer),
        Submission::Waiting
    );
    assert_eq!(
        backend(&mut runtime).input_responses[1].answers,
        Some(vec![vec!["edited answer".into()]])
    );
    assert!(
        input
            .resolve(
                runtime.epoch(),
                "first",
                QuestionResolution::Submitted {
                    message: None,
                    started_turn: false
                }
            )
            .is_some()
    );
    assert!(!input.submission_failed(runtime.epoch(), "first", "late failure".into()));
    assert_eq!(input.batches()[0].status(), QuestionStatus::Submitted);
}

#[test]
fn rejected_queue_write_keeps_the_draft_pending() {
    let mut runtime = runtime(AgentKind::Codex, "thread");
    let mut input = SessionInput::default();

    input.restore(&mut runtime);

    let key = receive(&mut input, &runtime, "request", QuestionMode::Async);

    backend(&mut runtime).input_result = Err("not connected".into());

    assert_eq!(
        submit(&mut input, &mut runtime, key, QuestionAction::Answer),
        Submission::Failed
    );
    assert!(backend(&mut runtime).input_responses.is_empty());
    assert_eq!(input.batches()[0].status(), QuestionStatus::Pending);
    assert_eq!(
        input.batches()[0].error(),
        Some(&QuestionError::Rejected("not connected".into()))
    );

    backend(&mut runtime).input_result = Ok(());

    assert_eq!(
        submit(&mut input, &mut runtime, key, QuestionAction::Answer),
        Submission::Waiting
    );
}

#[test]
fn duplicate_requests_preserve_drafts_and_replaced_positions_reject_old_keys() {
    let mut runtime = runtime(AgentKind::Codex, "thread");
    let mut input = SessionInput::default();

    input.restore(&mut runtime);

    let key = receive(&mut input, &runtime, "request", QuestionMode::Async);

    input.draft_mut(key).unwrap().set_text(0, "keep me".into());

    assert!(
        input
            .receive(&runtime, request("request", QuestionMode::Blocking))
            .is_none()
    );
    assert_eq!(input.batches()[0].text(0), "keep me");

    input.clear_questions();

    let replacement = receive(&mut input, &runtime, "request", QuestionMode::Async);

    assert_ne!(key, replacement);
    assert!(input.draft_mut(key).is_none());
    assert_eq!(
        submit(&mut input, &mut runtime, key, QuestionAction::Answer),
        Submission::Ignored
    );

    let index = input.receive_legacy(vec![question("Old", false, &["one"])]);
    let old = input.batches()[index].key();

    assert_eq!(
        input.receive_legacy(vec![question("New", false, &["two"])]),
        index
    );
    assert!(input.draft_mut(old).is_none());
}

#[test]
fn async_defaults_do_not_send_until_explicit_submission_and_blocking_requires_an_answer() {
    let mut runtime = runtime(AgentKind::Codex, "thread");
    let mut input = SessionInput::default();

    input.restore(&mut runtime);

    let asynchronous = receive(&mut input, &runtime, "async", QuestionMode::Async);
    let blocking = receive(&mut input, &runtime, "blocking", QuestionMode::Blocking);

    assert!(input.batches()[0].is_selected(0, 0));
    assert!(!input.batches()[1].is_complete());
    assert!(backend(&mut runtime).input_responses.is_empty());
    assert_eq!(
        submit(&mut input, &mut runtime, blocking, QuestionAction::Answer),
        Submission::Ignored
    );
    assert_eq!(
        submit(&mut input, &mut runtime, asynchronous, QuestionAction::Skip),
        Submission::Settled
    );
    assert_eq!(input.batches()[0].status(), QuestionStatus::Skipped);
}

#[test]
fn optional_timeout_only_skips_untouched_pending_requests_after_the_deadline() {
    let mut runtime = runtime(AgentKind::Codex, "thread");
    let mut input = SessionInput::default();

    input.restore(&mut runtime);

    let key = receive(&mut input, &runtime, "untouched", QuestionMode::Optional);
    let touched = receive(&mut input, &runtime, "touched", QuestionMode::Optional);

    input.draft_mut(touched).unwrap().touch();

    assert_eq!(
        submit(&mut input, &mut runtime, key, QuestionAction::Timeout),
        Submission::Ignored
    );

    let later = Instant::now() + Duration::from_secs(121);

    assert_eq!(
        input.submit(
            &mut runtime,
            touched,
            QuestionAction::Timeout,
            &ThreadSettings::default(),
            later
        ),
        Submission::Ignored
    );
    assert_eq!(
        input.submit(
            &mut runtime,
            key,
            QuestionAction::Timeout,
            &ThreadSettings::default(),
            later
        ),
        Submission::Waiting
    );
    assert_eq!(
        input.submit(
            &mut runtime,
            key,
            QuestionAction::Timeout,
            &ThreadSettings::default(),
            later
        ),
        Submission::Ignored
    );
    assert_eq!(
        backend(&mut runtime).input_responses,
        vec![InputResponse {
            id: Some("untouched".into()),
            answers: None
        }]
    );
}

#[test]
fn reconnect_restores_only_async_drafts_for_the_same_provider_and_thread() {
    let mut runtime = runtime(AgentKind::Codex, "thread");
    let mut input = SessionInput::default();

    input.restore(&mut runtime);

    let key = receive(&mut input, &runtime, "async", QuestionMode::Async);

    receive(&mut input, &runtime, "blocking", QuestionMode::Blocking);
    input.receive_legacy(vec![question("Legacy", false, &["yes"])]);

    input
        .draft_mut(key)
        .unwrap()
        .set_text(0, "saved draft".into());

    assert_eq!(
        submit(&mut input, &mut runtime, key, QuestionAction::Answer),
        Submission::Waiting
    );

    input.disconnect();

    assert!(
        input
            .resolve(runtime.epoch(), "async", QuestionResolution::Expired)
            .is_none()
    );

    input.restore(&mut runtime);

    assert_eq!(input.batches()[0].text(0), "saved draft");
    assert_eq!(input.batches()[0].status(), QuestionStatus::Pending);
    assert_eq!(
        input.batches()[0].error(),
        Some(&QuestionError::Disconnected)
    );
    assert_eq!(input.batches()[1].status(), QuestionStatus::Expired);
    assert_eq!(input.batches()[2].status(), QuestionStatus::Expired);
    assert_eq!(backend(&mut runtime).restored_questions.len(), 1);

    backend(&mut runtime).recovery = Some(RecoveryIdentity::new(AgentKind::DeepSeek, "thread"));
    input.restore(&mut runtime);

    assert_eq!(input.batches()[0].status(), QuestionStatus::Expired);
    assert!(backend(&mut runtime).restored_questions.is_empty());
}

#[test]
fn old_epoch_completions_cannot_settle_or_reopen_a_restored_draft() {
    let mut runtime = runtime(AgentKind::Codex, "thread");
    let mut input = SessionInput::default();

    input.restore(&mut runtime);

    let key = receive(&mut input, &runtime, "request", QuestionMode::Async);

    assert_eq!(
        submit(&mut input, &mut runtime, key, QuestionAction::Answer),
        Submission::Waiting
    );

    let old_epoch = runtime.epoch();
    let epoch = runtime.begin_start();

    input.starting(epoch);
    runtime.ready();
    input.restore(&mut runtime);

    assert_eq!(
        submit(&mut input, &mut runtime, key, QuestionAction::Answer),
        Submission::Waiting
    );
    assert!(
        input
            .resolve(old_epoch, "request", QuestionResolution::Expired)
            .is_none()
    );
    assert!(!input.submission_failed(old_epoch, "request", "old failure".into()));
    assert_eq!(input.batches()[0].status(), QuestionStatus::Submitting);
    assert!(
        input
            .resolve(epoch, "request", QuestionResolution::Expired)
            .is_some()
    );
}

#[test]
fn completion_is_applied_once_and_waiting_ends_only_after_the_last_blocking_request() {
    let mut runtime = runtime(AgentKind::Codex, "thread");
    let mut input = SessionInput::default();

    input.restore(&mut runtime);
    receive(&mut input, &runtime, "first", QuestionMode::Blocking);
    receive(&mut input, &runtime, "second", QuestionMode::Blocking);

    assert!(
        !input
            .resolve(runtime.epoch(), "first", QuestionResolution::Expired)
            .unwrap()
            .waiting_finished
    );

    let completion = input
        .resolve(
            runtime.epoch(),
            "second",
            QuestionResolution::Submitted {
                message: Some("answer".into()),
                started_turn: true,
            },
        )
        .unwrap();

    assert!(completion.waiting_finished);
    assert!(completion.started_turn);
    assert_eq!(completion.message.as_deref(), Some("answer"));
    assert!(
        input
            .resolve(
                runtime.epoch(),
                "second",
                QuestionResolution::Submitted {
                    message: Some("duplicate".into()),
                    started_turn: true
                }
            )
            .is_none()
    );
}

#[test]
fn settled_secret_answers_are_removed_but_retryable_answers_are_retained() {
    let mut runtime = runtime(AgentKind::Codex, "thread");
    let mut input = SessionInput::default();

    input.restore(&mut runtime);

    let mut request = request("secret", QuestionMode::Async);

    request.questions[0].input = QuestionInput::Secret;

    let index = input.receive(&runtime, request).unwrap();
    let key = input.batches()[index].key();

    input
        .draft_mut(key)
        .unwrap()
        .set_text(0, "sensitive".into());

    assert_eq!(
        submit(&mut input, &mut runtime, key, QuestionAction::Answer),
        Submission::Waiting
    );
    assert!(input.submission_failed(runtime.epoch(), "secret", "retry".into()));
    assert_eq!(input.batches()[index].text(0), "sensitive");
    assert_eq!(
        submit(&mut input, &mut runtime, key, QuestionAction::Answer),
        Submission::Waiting
    );

    input
        .resolve(
            runtime.epoch(),
            "secret",
            QuestionResolution::Submitted {
                message: None,
                started_turn: false,
            },
        )
        .unwrap();

    assert_eq!(input.batches()[index].text(0), "");
    assert!(
        !input
            .draft_mut(key)
            .unwrap()
            .set_text(0, "late editor value".into())
    );
}

#[test]
fn approvals_distinguish_rejection_immediate_settlement_and_confirmation() {
    let mut runtime = runtime(AgentKind::Codex, "thread");
    let mut input = SessionInput::default();

    input.restore(&mut runtime);
    input.ask_approval("Run command".into());

    assert_eq!(
        input.respond_approval(&mut runtime, "accept"),
        ApprovalOutcome::Rejected
    );
    assert_eq!(input.approval(), Some("Run command"));

    backend(&mut runtime).approval_accepted = true;

    assert_eq!(
        input.respond_approval(&mut runtime, "accept"),
        ApprovalOutcome::Settled
    );
    assert!(input.approval().is_none());
    assert!(!input.resolve_approval(runtime.epoch()));

    backend(&mut runtime).approval_waits = true;
    input.ask_approval("Another command".into());

    assert_eq!(
        input.respond_approval(&mut runtime, "decline"),
        ApprovalOutcome::Waiting
    );
    assert_eq!(
        input.respond_approval(&mut runtime, "accept"),
        ApprovalOutcome::Ignored
    );
    assert_eq!(backend(&mut runtime).approval_responses.len(), 2);
    assert!(!input.resolve_approval(runtime.epoch() + 1));
    assert!(input.approval().is_some());
    assert!(input.resolve_approval(runtime.epoch()));
    assert!(input.approval().is_none());
}

#[test]
fn disconnect_clears_approval_and_prevents_writes_to_a_retired_backend() {
    let mut runtime = runtime(AgentKind::Codex, "thread");
    let mut input = SessionInput::default();

    input.restore(&mut runtime);

    let key = receive(&mut input, &runtime, "request", QuestionMode::Async);

    input.ask_approval("Run command".into());
    input.starting(runtime.begin_start());

    assert!(input.approval().is_none());
    assert_eq!(
        input.respond_approval(&mut runtime, "accept"),
        ApprovalOutcome::Ignored
    );
    assert_eq!(
        submit(&mut input, &mut runtime, key, QuestionAction::Answer),
        Submission::Ignored
    );
    assert!(backend(&mut runtime).input_responses.is_empty());
}
