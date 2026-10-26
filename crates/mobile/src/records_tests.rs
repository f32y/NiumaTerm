use nmt_agent::chat::ModelInfo;
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
