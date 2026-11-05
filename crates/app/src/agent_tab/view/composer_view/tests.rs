use gpui::prelude::*;
use gpui::{
    Context, Entity, Render, TestAppContext, VisualTestContext, Window, div, px, relative, size,
};
use gpui_component::Root;
use nmt_agent::AgentWorkspace;
use nmt_agent::session::AgentKind;
use nmt_config::profile::AgentProfile;

use crate::agent_tab::execution::AgentSession;
use crate::agent_tab::settings::AgentSettings;
use crate::agent_tab::{AgentPane, RecentSessionsMode};

struct PaneHost(Entity<AgentPane>);

impl Render for PaneHost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        // A proportional sidebar produces fractional pane origins and widths.
        div().size_full().pl(relative(0.173)).child(self.0.clone())
    }
}

#[gpui::test]
fn composer_positions_stay_stable_after_typing_and_repainting(cx: &mut TestAppContext) {
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
            let owner = AgentSession::create(
                AgentProfile {
                    name: "Claude Test".into(),
                    kind: AgentKind::Claude,
                    executable: "missing-layout-agent.exe".into(),
                    ..AgentProfile::default()
                },
                AgentWorkspace::default(),
                None,
                cx,
            );

            let mut pane = AgentPane::attach(&owner, window, cx);

            pane.owned_session = Some(owner);

            pane
        });

        pane = Some(agent.clone());

        let host = cx.new(|_| PaneHost(agent));

        Root::new(host, window, cx)
    });

    let pane = pane.unwrap();
    let cx: &mut VisualTestContext = cx;

    pane.update(cx, |pane, cx| {
        pane.history_ui.mode = RecentSessionsMode::Hidden;

        cx.notify();
    });

    cx.run_until_parked();

    for width in [800., 801., 803., 1001., 1203.] {
        cx.simulate_resize(size(px(width), px(800.)));

        cx.update(|window, cx| {
            pane.update(cx, |pane, cx| {
                pane.input.update(cx, |input, cx| {
                    input.set_value("", window, cx);
                    input.replace("abcde", window, cx);
                    input.focus(window, cx);
                });

                cx.notify();
            });

            let _ = window.draw(cx);
        });

        let model = cx.debug_bounds("agent-model").unwrap();

        let cursor = pane.read_with(cx, |pane, cx| {
            pane.input.read(cx).cursor_layout().unwrap().0
        });

        for frame in 0..9 {
            cx.run_until_parked();

            cx.update(|window, cx| {
                pane.update(cx, |pane, cx| {
                    // Exercise clean frames, cursor redraws and session redraws.
                    match frame % 3 {
                        0 => {}
                        1 => pane.input.update(cx, |_, cx| cx.notify()),
                        _ => cx.notify(),
                    }
                });

                let _ = window.draw(cx);
            });

            let next_model = cx.debug_bounds("agent-model").unwrap();

            let next_cursor = pane.read_with(cx, |pane, cx| {
                pane.input.read(cx).cursor_layout().unwrap().0
            });

            assert_eq!(
                (next_model.origin.x, next_cursor.origin.x),
                (model.origin.x, cursor.origin.x),
                "model and cursor: width {width}, frame {frame}"
            );
        }
    }
}
