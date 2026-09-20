use gpui::{AppContext as _, Entity, TestAppContext};
use nmt_agent::AgentWorkspace;
use nmt_config::profile::{AgentKind, AgentProfile};

use crate::agent_tab::AgentPane;
use crate::agent_tab::settings::AgentSettings;
use crate::agent_tab::thread_controls::defaults::remember_defaults;

/// Two tabs opened from one profile, so a pick made in one would be visible in
/// the other if the picks were kept anywhere but on the tab.
fn open_two_panes(cx: &mut TestAppContext) -> (Entity<AgentPane>, Entity<AgentPane>) {
    let profile = AgentProfile {
        name: "Shared Profile".into(),
        kind: AgentKind::Codex,
        executable: "missing-codex.exe".into(),
        ..AgentProfile::default()
    };

    let second = profile.clone();

    cx.update(|cx| {
        gpui_component::init(cx);

        cx.set_global(AgentSettings::default());

        let mut panes = Vec::new();

        for profile in [profile, second] {
            cx.open_window(Default::default(), |window, cx| {
                let pane =
                    cx.new(|cx| AgentPane::new(profile, AgentWorkspace::default(), window, cx));

                panes.push(pane.clone());

                cx.new(|cx| gpui_component::Root::new(pane, window, cx))
            })
            .expect("open Agent test window");
        }

        (panes[0].clone(), panes[1].clone())
    })
}

#[gpui::test]
fn a_pick_stays_on_the_tab_that_made_it(cx: &mut TestAppContext) {
    let (first, second) = open_two_panes(cx);

    cx.update(|cx| {
        first.update(cx, |pane, cx| {
            pane.session
                .borrow_mut()
                .controls
                .set_model("picked-model".to_string());

            remember_defaults(pane, cx);
        });

        let remembered = first
            .read(cx)
            .host
            .upgrade()
            .expect("first session")
            .read(cx)
            .remembered_settings()
            .and_then(|settings| settings.model.clone());

        assert_eq!(remembered.as_deref(), Some("picked-model"));

        // The second tab runs the same profile and was never adjusted, so it
        // still starts from what its launch profile resolves.
        assert!(
            second
                .read(cx)
                .host
                .upgrade()
                .expect("second session")
                .read(cx)
                .remembered_settings()
                .is_none()
        );
    });
}
