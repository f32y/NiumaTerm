use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext, px};
use gpui_component::Root;
use nmt_agent::AgentWorkspace;
use nmt_agent::chat::{Event, SlashCommandOutcome};
use nmt_agent::progress::{GoalStatus, Task, TaskList, TaskStatus};
use nmt_agent::session::test_support::TestBackend;
use nmt_agent::session::{AgentKind, Backend};
use nmt_config::profile::{AgentProfile, AgentProfileKind};

use crate::agent_tab::settings::AgentSettings;
use crate::agent_tab::tests::deliver_session_event;
use crate::agent_tab::{AgentPane, AgentThreadDefaults, RecentSessionsMode};

#[gpui::test]
fn progress_panel_is_narrower_and_expands_above_the_composer(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);

        cx.set_global(AgentSettings::default());
        cx.set_global(AgentThreadDefaults::default());
    });

    let mut pane: Option<Entity<AgentPane>> = None;

    let (_, cx) = cx.add_window_view(|window, cx| {
        let agent = cx.new(|cx| {
            AgentPane::new(
                AgentProfile {
                    name: "Progress Test".into(),
                    kind: AgentProfileKind::Codex,
                    executable: "missing-progress-agent.exe".into(),
                    ..AgentProfile::default()
                },
                AgentWorkspace::default(),
                window,
                cx,
            )
        });

        pane = Some(agent.clone());

        Root::new(agent, window, cx)
    });

    let pane = pane.unwrap();
    let cx: &mut VisualTestContext = cx;

    pane.update(cx, |pane, cx| {
        let mut session = pane.session.borrow_mut();

        let epoch = session.starting(None).epoch;

        let backend = TestBackend::new([], SlashCommandOutcome::Accepted, vec![])
            .with_recovery(AgentKind::Codex, "progress");

        session.install(epoch, Ok(Backend::Test(backend)));
        session.runtime.ready();

        pane.history_ui.mode = RecentSessionsMode::Hidden;

        cx.notify();
    });

    deliver_session_event(
        &pane,
        Event::GoalUpdated(Some(GoalStatus {
            objective: "Build both targets and verify their output. ".repeat(30),
            phase: "active".into(),
            ..GoalStatus::default()
        })),
        cx,
    );

    deliver_session_event(
        &pane,
        Event::TaskListUpdated(TaskList {
            explanation: None,
            items: vec![Task {
                id: "one".into(),
                title: "Verify build output".into(),
                description: Some("Check both targets".into()),
                status: TaskStatus::InProgress,
                owner: None,
                blocked_by: vec![],
            }],
        }),
        cx,
    );

    cx.run_until_parked();

    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    let composer = cx.debug_bounds("agent-progress-composer").unwrap();
    let collapsed = cx.debug_bounds("agent-progress-panel").unwrap();

    assert!((collapsed.size.width - composer.size.width * 0.95).abs() < px(1.));
    assert!((collapsed.center().x - composer.center().x).abs() < px(1.));
    assert!(collapsed.top() < composer.top());
    assert!(cx.debug_bounds("agent-progress-details").is_none());

    let toggle = cx.debug_bounds("agent-progress-toggle").unwrap();

    assert!(toggle.center().x > collapsed.center().x);

    cx.simulate_click(toggle.center(), Default::default());
    cx.run_until_parked();

    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    let expanded = cx.debug_bounds("agent-progress-panel").unwrap();
    let details = cx.debug_bounds("agent-progress-details").unwrap();
    let objective = cx.debug_bounds("agent-progress-objective").unwrap();

    assert!(expanded.size.height > collapsed.size.height);
    assert!(
        objective.size.height > px(30.),
        "the full objective must wrap"
    );
    assert!(details.size.height <= px(260.));
    assert_eq!(
        cx.debug_bounds("agent-progress-composer").unwrap(),
        composer
    );

    let toggle = cx.debug_bounds("agent-progress-toggle").unwrap();

    cx.simulate_click(toggle.center(), Default::default());
    cx.run_until_parked();

    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    assert!(cx.debug_bounds("agent-progress-details").is_none());
}
