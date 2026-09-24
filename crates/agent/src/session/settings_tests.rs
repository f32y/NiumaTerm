use crate::chat::ThreadSettings;
use crate::session::AgentKind;
use crate::session::restore::SettingsSeed;
use crate::session::settings::ConversationSettings;

fn seeded(seed: SettingsSeed) -> ConversationSettings {
    ConversationSettings {
        seed,
        ..ConversationSettings::default()
    }
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
        agent_preset: None,
    };

    let stored = ThreadSettings {
        model: Some("local-model".into()),
        approval: Some("on-request".into()),
        approvals_reviewer: Some("auto_review".into()),
        sandbox: Some("workspaceWrite".into()),
        effort: Some("high".into()),
        tier: None,
        agent_preset: None,
    };

    let mut controls = seeded(SettingsSeed::Reviewer);

    controls.ready(AgentKind::Codex, backend, Some(&stored), None, None);

    assert_eq!(
        controls.settings,
        ThreadSettings {
            model: Some("thread-model".into()),
            approval: Some("never".into()),
            approvals_reviewer: Some("auto_review".into()),
            sandbox: Some("readOnly".into()),
            effort: Some("low".into()),
            tier: Some("priority".into()),
            agent_preset: None,
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

    let mut controls = seeded(SettingsSeed::Defaults);

    controls.ready(
        AgentKind::Claude,
        backend.clone(),
        Some(&local),
        Some("profile-model"),
        None,
    );

    let initial = controls.settings.clone();

    assert_eq!(initial.model.as_deref(), Some("profile-model"));
    assert_eq!(initial.approval.as_deref(), Some("auto"));
    assert_eq!(initial.effort.as_deref(), Some("high"));

    // Claude reports Ready again during its first turn; that confirmation
    // keeps the controls in use rather than the ones the CLI reports.
    controls.ready(
        AgentKind::Claude,
        backend,
        Some(&local),
        Some("profile-model"),
        None,
    );

    assert_eq!(controls.settings, initial);
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

    let mut controls = seeded(SettingsSeed::Defaults);

    controls.ready(
        AgentKind::DeepSeek,
        backend,
        Some(&local),
        None,
        Some("max"),
    );

    assert_eq!(controls.settings.effort.as_deref(), Some("max"));
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

    let mut controls = seeded(SettingsSeed::Defaults);

    controls.ready(AgentKind::DeepSeek, backend, Some(&local), None, None);

    assert_eq!(controls.settings.effort.as_deref(), Some("medium"));
}
