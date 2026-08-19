use nmt_agent_utils::background_task::{
    BackgroundTaskDiscoveryState, BackgroundTaskKey, BackgroundTaskRegistry,
    BackgroundTaskSnapshot, BackgroundTaskState, BackgroundTaskUpdate,
};
use nmt_agent_utils::chat::ThreadSettings;

use crate::agent_pane::session::events::resolve_ready_settings;
use crate::agent_pane::session::{scoped_background_tasks, tab_title_from_prompt};

fn snapshot_for(parent: BackgroundTaskKey) -> BackgroundTaskSnapshot {
    let mut registry = BackgroundTaskRegistry::new(parent);
    registry.apply(
        BackgroundTaskKey::codex("child-1"),
        BackgroundTaskUpdate::state(BackgroundTaskState::Working),
    );
    registry.snapshot()
}

#[test]
fn a_snapshot_is_shown_only_for_the_session_it_describes() {
    let codex = BackgroundTaskKey::codex("thread-a");
    let snapshot = snapshot_for(codex.clone());

    assert!(scoped_background_tasks(Some(&codex), Some(&snapshot)).is_some());
    assert!(
        scoped_background_tasks(Some(&BackgroundTaskKey::codex("thread-b")), Some(&snapshot))
            .is_none()
    );
    assert!(
        scoped_background_tasks(
            Some(&BackgroundTaskKey::claude_code("thread-a")),
            Some(&snapshot)
        )
        .is_none(),
        "a Claude session must not adopt a Codex thread's rows"
    );
    assert!(
        scoped_background_tasks(None, Some(&snapshot)).is_none(),
        "an unsupported or not-yet-started pane shows no rows"
    );
}

#[test]
fn a_later_snapshot_replaces_the_previous_one_and_carries_its_activity() {
    let parent = BackgroundTaskKey::claude_code("session-1");
    let mut registry = BackgroundTaskRegistry::new(parent.clone());
    registry.apply(
        BackgroundTaskKey::claude_code("task-1"),
        BackgroundTaskUpdate::state(BackgroundTaskState::Working),
    );
    let first = registry.snapshot();

    registry.apply(
        BackgroundTaskKey::claude_code("task-1"),
        BackgroundTaskUpdate::state(BackgroundTaskState::Done),
    );
    let second = registry.snapshot();

    assert_eq!(first.active_count(), 1);
    assert_eq!(second.active_count(), 0);
    assert!(second.activity > first.activity);
    assert_eq!(
        scoped_background_tasks(Some(&parent), Some(&second)),
        Some(&second)
    );
}

#[test]
fn a_failed_refresh_reports_unavailable_without_dropping_known_rows() {
    let parent = BackgroundTaskKey::codex("thread-a");
    let mut registry = BackgroundTaskRegistry::new(parent);
    registry.apply(
        BackgroundTaskKey::codex("child-1"),
        BackgroundTaskUpdate::state(BackgroundTaskState::Working),
    );
    registry.set_discovery(BackgroundTaskDiscoveryState::Unavailable {
        message: "thread/list failed".into(),
    });

    let snapshot = registry.snapshot();
    assert_eq!(snapshot.tasks.len(), 1);
    assert_eq!(snapshot.active_count(), 1);
    assert!(matches!(
        snapshot.discovery,
        BackgroundTaskDiscoveryState::Unavailable { .. }
    ));
}

#[test]
fn resumed_codex_thread_uses_only_the_locally_remembered_reviewer() {
    let backend = ThreadSettings {
        model: Some("thread-model".into()),
        approval: Some("never".into()),
        approvals_reviewer: Some("user".into()),
        sandbox: Some("readOnly".into()),
        effort: Some("low".into()),
        tier: Some("priority".into()),
    };
    let stored = ThreadSettings {
        model: Some("local-model".into()),
        approval: Some("on-request".into()),
        approvals_reviewer: Some("auto_review".into()),
        sandbox: Some("workspaceWrite".into()),
        effort: Some("high".into()),
        tier: None,
    };

    assert_eq!(
        resolve_ready_settings(backend, Some(&stored), false, true, None, None),
        ThreadSettings {
            model: Some("thread-model".into()),
            approval: Some("never".into()),
            approvals_reviewer: Some("auto_review".into()),
            sandbox: Some("readOnly".into()),
            effort: Some("low".into()),
            tier: Some("priority".into()),
        }
    );
}

#[test]
fn claude_profile_and_local_settings_survive_later_ready_events() {
    let backend = ThreadSettings {
        model: Some("agent-model".into()),
        approval: Some("default".into()),
        effort: None,
        ..ThreadSettings::default()
    };
    let local = ThreadSettings {
        model: Some("remembered-model".into()),
        approval: Some("auto".into()),
        effort: Some("high".into()),
        ..ThreadSettings::default()
    };
    let initial = resolve_ready_settings(
        backend.clone(),
        Some(&local),
        true,
        false,
        Some("profile-model"),
        None,
    );

    assert_eq!(initial.model.as_deref(), Some("profile-model"));
    assert_eq!(initial.approval.as_deref(), Some("auto"));
    assert_eq!(initial.effort.as_deref(), Some("high"));
    assert_eq!(
        resolve_ready_settings(backend, Some(&initial), true, false, None, None),
        initial
    );
}

#[test]
fn a_pinned_profile_effort_outranks_the_thread_and_the_remembered_pick() {
    let backend = ThreadSettings {
        effort: Some("low".into()),
        ..ThreadSettings::default()
    };
    let local = ThreadSettings {
        effort: Some("medium".into()),
        ..ThreadSettings::default()
    };

    let resolved = resolve_ready_settings(backend, Some(&local), true, false, None, Some("max"));

    assert_eq!(resolved.effort.as_deref(), Some("max"));
}

#[test]
fn no_pinned_effort_leaves_the_remembered_pick_in_place() {
    let backend = ThreadSettings {
        effort: Some("low".into()),
        ..ThreadSettings::default()
    };
    let local = ThreadSettings {
        effort: Some("medium".into()),
        ..ThreadSettings::default()
    };

    let resolved = resolve_ready_settings(backend, Some(&local), true, false, None, None);

    assert_eq!(resolved.effort.as_deref(), Some("medium"));
}

#[test]
fn a_pending_snapshot_claims_only_the_prompts_it_stopped_naming() {
    use std::collections::VecDeque;

    use nmt_agent_utils::chat::QueuedPrompt;

    use crate::agent_pane::session::conversation::claimed_prompts;

    let backend = |id: &str, text: &str| QueuedPrompt {
        id: Some(id.into()),
        text: text.into(),
    };

    let held = VecDeque::from([
        backend("msg-1", "run the tests"),
        backend("msg-2", "then push"),
        // Sent a moment ago; the backend has not named it yet.
        QueuedPrompt::local("and tag it".into()),
    ]);

    // The snapshot that first names the local row must not read as a claim:
    // the prompt is still waiting, and publishing it would show it as sent.
    assert_eq!(
        claimed_prompts(
            &held,
            &[
                backend("msg-1", "run the tests"),
                backend("msg-2", "then push"),
                backend("msg-3", "and tag it"),
            ],
        ),
        Vec::<String>::new(),
    );

    // The agent took the head of the queue; that row is now due.
    assert_eq!(
        claimed_prompts(
            &held,
            &[
                backend("msg-2", "then push"),
                backend("msg-3", "and tag it")
            ],
        ),
        vec!["run the tests".to_string()],
    );

    // An emptied queue claims everything still held, in queue order.
    assert_eq!(
        claimed_prompts(&held, &[]),
        vec![
            "run the tests".to_string(),
            "then push".to_string(),
            "and tag it".to_string(),
        ],
    );
}

#[test]
fn a_prompt_names_its_tab_by_its_first_real_line() {
    assert_eq!(
        tab_title_from_prompt(
            "
  Fix the flaky auth test
and the retry loop"
        ),
        Some("Fix the flaky auth test".to_string())
    );

    // A slash command instructs the CLI instead of stating a subject, and the
    // settings controls send some of them for the user.
    assert_eq!(tab_title_from_prompt("/effort high"), None);
    assert_eq!(
        tab_title_from_prompt(
            "   
	 "
        ),
        None
    );

    let long = "x".repeat(200);
    assert_eq!(
        tab_title_from_prompt(&long).map(|t| t.chars().count()),
        Some(60)
    );
}
