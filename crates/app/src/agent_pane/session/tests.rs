use nmt_agent_utils::chat::ThreadSettings;

use crate::agent_pane::session::events::overlay_remembered_settings;

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
        overlay_remembered_settings(backend, Some(&stored), false, true),
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
