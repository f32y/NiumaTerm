use nmt_agent_utils::chat::ThreadSettings;

use crate::agent_pane::session::events::resolve_ready_settings;

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
        resolve_ready_settings(backend, Some(&stored), false, true, None),
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
    );

    assert_eq!(initial.model.as_deref(), Some("profile-model"));
    assert_eq!(initial.approval.as_deref(), Some("auto"));
    assert_eq!(initial.effort.as_deref(), Some("high"));
    assert_eq!(
        resolve_ready_settings(backend, Some(&initial), true, false, None),
        initial
    );
}
