use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext, point, px};
use gpui_component::Root;
use nmt_agent::AgentWorkspace;
use nmt_agent::chat::{Event, QueuedPrompt, SlashCommandOutcome};
use nmt_agent::progress::{GoalStatus, Task, TaskList, TaskStatus};
use nmt_agent::session::test_support::TestBackend;
use nmt_agent::session::{AgentKind, Backend};
use nmt_config::profile::AgentProfile;

use crate::agent_tab::settings::AgentSettings;
use crate::agent_tab::tests::deliver_session_event;
use crate::agent_tab::{AgentPane, RecentSessionsMode};

#[gpui::test]
fn progress_panel_is_narrower_and_expands_above_the_composer(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);

        // Reduced motion settles each toggle within one frame; the ramp runs on
        // wall-clock time, which a test frame does not advance.
        cx.set_global(AgentSettings {
            reduce_motion: true,
            ..AgentSettings::default()
        });
    });

    let mut pane: Option<Entity<AgentPane>> = None;

    let (_, cx) = cx.add_window_view(|window, cx| {
        let agent = cx.new(|cx| {
            AgentPane::new(
                AgentProfile {
                    name: "Progress Test".into(),
                    kind: AgentKind::Codex,
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

        let epoch = session.starting(None);

        let backend = TestBackend::new([], SlashCommandOutcome::Accepted, vec![])
            .with_recovery(AgentKind::Codex, "progress");

        session.install(epoch, Ok(Backend::Test(backend)));
        session.runtime_mut().ready();

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
    let header = cx.debug_bounds("agent-progress-header").unwrap();
    let transcript = cx.debug_bounds("agent-progress-transcript").unwrap();

    // The panel's lower edge runs behind the card, so the header is centred in
    // the band between the panel's top and the card's top.
    let visible_center = (collapsed.top() + composer.top()) / 2.;

    assert!((header.center().y - visible_center).abs() < px(1.));
    assert!(transcript.bottom() <= collapsed.top());

    assert!((collapsed.size.width - composer.size.width * 0.95).abs() < px(1.));
    assert!((collapsed.center().x - composer.center().x).abs() < px(1.));
    assert!(collapsed.top() < composer.top());
    assert!(cx.debug_bounds("agent-progress-details").is_none());

    let toggle = cx.debug_bounds("agent-progress-toggle").unwrap();

    assert!(toggle.center().x > collapsed.center().x);

    cx.simulate_click(header.center(), Default::default());
    cx.run_until_parked();

    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    let expanded = cx.debug_bounds("agent-progress-panel").unwrap();
    let details = cx.debug_bounds("agent-progress-details").unwrap();
    let objective = cx.debug_bounds("agent-progress-objective").unwrap();

    assert!(expanded.size.height > collapsed.size.height);
    assert_eq!(
        cx.debug_bounds("agent-progress-header").unwrap(),
        header,
        "opening grows the panel upward and leaves the header where it was clicked"
    );
    assert!(details.bottom() <= header.top());

    // Opening the details takes space from the transcript instead of covering
    // its last rows.
    let pushed = cx.debug_bounds("agent-progress-transcript").unwrap();

    assert!(pushed.bottom() <= expanded.top());
    assert!(pushed.bottom() < transcript.bottom());
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

    let padding = point(header.left() + px(2.), header.center().y);

    for position in [padding, header.center(), toggle.center(), padding] {
        let was_expanded = pane.read_with(cx, |pane, _| pane.progress_panel.expanded);

        cx.simulate_click(position, Default::default());
        cx.run_until_parked();

        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        assert_eq!(
            cx.debug_bounds("agent-progress-details").is_some(),
            !was_expanded,
            "each click on the summary, padding, or arrow toggles the details once"
        );
    }
}

#[gpui::test]
fn a_finished_task_list_leaves_the_composer(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);

        cx.set_global(AgentSettings {
            reduce_motion: true,
            ..AgentSettings::default()
        });
    });

    let mut pane: Option<Entity<AgentPane>> = None;

    let (_, cx) = cx.add_window_view(|window, cx| {
        let agent = cx.new(|cx| {
            AgentPane::new(
                AgentProfile {
                    name: "Progress Test".into(),
                    kind: AgentKind::Codex,
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

        let epoch = session.starting(None);

        let backend = TestBackend::new([], SlashCommandOutcome::Accepted, vec![])
            .with_recovery(AgentKind::Codex, "progress");

        session.install(epoch, Ok(Backend::Test(backend)));
        session.runtime_mut().ready();

        pane.history_ui.mode = RecentSessionsMode::Hidden;

        cx.notify();
    });

    let list = |status: TaskStatus| TaskList {
        explanation: None,
        items: vec![Task {
            id: "one".into(),
            title: "Verify build output".into(),
            description: None,
            status,
            owner: None,
            blocked_by: vec![],
        }],
    };

    deliver_session_event(
        &pane,
        Event::TaskListUpdated(list(TaskStatus::InProgress)),
        cx,
    );

    cx.run_until_parked();

    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    assert!(cx.debug_bounds("agent-progress-panel").is_some());
    assert!(cx.debug_bounds("agent-task-list").is_none());

    deliver_session_event(
        &pane,
        Event::TaskListUpdated(list(TaskStatus::Completed)),
        cx,
    );

    cx.run_until_parked();

    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    assert!(
        cx.debug_bounds("agent-progress-panel").is_none(),
        "a finished list has nothing left to show above the composer"
    );
    assert!(
        cx.debug_bounds("agent-task-list").is_some(),
        "the finished list is read in the transcript instead"
    );
}

#[gpui::test]
fn queued_prompts_sit_in_a_strip_above_the_composer(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);

        cx.set_global(AgentSettings {
            reduce_motion: true,
            ..AgentSettings::default()
        });
    });

    let mut pane: Option<Entity<AgentPane>> = None;

    let (_, cx) = cx.add_window_view(|window, cx| {
        let agent = cx.new(|cx| {
            AgentPane::new(
                AgentProfile {
                    name: "Queue Test".into(),
                    kind: AgentKind::Codex,
                    executable: "missing-queue-agent.exe".into(),
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

        let epoch = session.starting(None);

        let backend = TestBackend::new([], SlashCommandOutcome::Accepted, vec![])
            .with_recovery(AgentKind::Codex, "queue");

        session.install(epoch, Ok(Backend::Test(backend)));
        session.runtime_mut().ready();

        pane.history_ui.mode = RecentSessionsMode::Hidden;

        cx.notify();
    });

    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    assert!(cx.debug_bounds("agent-notice-panel").is_none());

    deliver_session_event(
        &pane,
        Event::QueuedPrompts(vec![QueuedPrompt {
            id: Some("queued-1".into()),
            text: "Then run the tests".into(),
        }]),
        cx,
    );

    cx.run_until_parked();

    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    let composer = cx.debug_bounds("agent-progress-composer").unwrap();
    let strip = cx.debug_bounds("agent-notice-panel").unwrap();

    assert!((strip.size.width - composer.size.width * 0.95).abs() < px(1.));
    assert!((strip.center().x - composer.center().x).abs() < px(1.));
    assert!(strip.top() < composer.top());
    assert!(
        strip.bottom() > composer.top(),
        "the strip's lower edge tucks behind the card"
    );
}
