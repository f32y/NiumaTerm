use std::time::{Duration, UNIX_EPOCH};

use nmt_agent::chat::{ModelInfo, SessionSummary};
use nmt_agent::session::AgentKind;
use nmt_agent::session::controller::SessionController;
use nmt_agent::session::view::ViewPublisher;

use crate::records::agent_state;

#[test]
fn model_choices_include_codex_levels_and_preserve_dsh_options() {
    let mut view = ViewPublisher::default().snapshot(&SessionController::new(AgentKind::Codex));

    view.slots.settings.settings.model = Some("selected".into());
    view.slots.settings.settings.effort = Some("high".into());

    view.slots.settings.models.push(ModelInfo {
        model: "selected".into(),
        display: "Selected".into(),
        tiers: Vec::new(),
        default_tier: None,
        efforts: Vec::new(),
    });

    view.slots.catalogs.kind = Some(AgentKind::Codex);

    let state = agent_state(Some(&view), None, None);

    assert_eq!(state.model.as_deref(), Some("selected"));
    assert_eq!(state.effort.as_deref(), Some("high"));
    assert_eq!(
        state.models[0].efforts,
        ["low", "medium", "high", "xhigh", "max", "ultra"]
    );

    view.slots.catalogs.kind = Some(AgentKind::DeepSeek);
    view.slots.settings.models[0].efforts = vec!["off".into(), "high".into(), "max".into()];

    let state = agent_state(Some(&view), None, None);

    assert_eq!(state.models[0].efforts, ["off", "high", "max"]);

    view.slots.settings.models[0].efforts.clear();

    let state = agent_state(Some(&view), None, None);

    assert!(state.models[0].efforts.is_empty());
}

#[test]
fn recent_sessions_preserve_host_identifiers_and_activity_times() {
    let mut controller = SessionController::new(AgentKind::DeepSeek);
    let mut publisher = ViewPublisher::default();

    controller.clear_listed_history();

    let view = publisher.snapshot(&controller);

    assert!(agent_state(Some(&view), None, None).history_loading);

    controller.list_history(vec![SessionSummary {
        id: "conversation-42".into(),
        title: "Previous work".into(),
        cwd: Some("/project".into()),
        branch: None,
        last_active: UNIX_EPOCH + Duration::from_secs(42),
        snippet: None,
        origin: None,
    }]);

    let view = publisher.snapshot(&controller);
    let state = agent_state(Some(&view), None, None);

    assert!(!state.history_loading);
    assert_eq!(state.recent_sessions[0].id, "conversation-42");
    assert_eq!(state.recent_sessions[0].title, "Previous work");
    assert_eq!(state.recent_sessions[0].cwd.as_deref(), Some("/project"));
    assert_eq!(state.recent_sessions[0].last_active, 42);
}
