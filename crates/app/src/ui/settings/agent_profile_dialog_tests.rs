use gpui::{
    AppContext as _, Context, IntoElement, ParentElement as _, Render, TestAppContext,
    VisualTestContext, Window, div,
};
use gpui_component::{Root, WindowExt as _};

use crate::ui::settings::agent_profile_dialog::{
    AgentProfileDraft, agent_profile_dialog, save_agent_profile_draft,
};
use crate::ui::settings::{AgentProfile, AppSettings};

struct DialogHost;

impl Render for DialogHost {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().children(Root::render_dialog_layer(window, cx))
    }
}

#[gpui::test]
fn profile_dialogs_keep_separate_drafts_and_release_cancelled_credentials(cx: &mut TestAppContext) {
    cx.set_global(AppSettings::default());

    let (first, second) = cx.update(|cx| {
        (
            cx.new(|_| AgentProfileDraft {
                profile: AgentProfile {
                    name: "Unsaved draft".into(),
                    api_key: "unsaved test key".into(),
                    ..AgentProfile::default()
                },
                ..AgentProfileDraft::default()
            }),
            cx.new(|_| AgentProfileDraft {
                profile: AgentProfile {
                    name: "Saved draft".into(),
                    api_key: "saved test key".into(),
                    ..AgentProfile::default()
                },
                ..AgentProfileDraft::default()
            }),
        )
    });

    let cancelled = first.downgrade();

    let window = cx.update(|cx| {
        cx.open_window(Default::default(), |window, cx| {
            gpui_component::init(cx);

            let empty = cx.new(|_| DialogHost);

            cx.new(|cx| Root::new(empty, window, cx))
        })
        .unwrap()
    });

    let mut cx = VisualTestContext::from_window(window.into(), cx);

    cx.update(move |window, cx| {
        window.open_dialog(cx, move |dialog, window, _| {
            agent_profile_dialog(dialog, &first, None, window)
        });

        save_agent_profile_draft(&second, cx);

        let profiles = &cx.global::<AppSettings>().config().agent_profiles.list;

        assert!(
            profiles
                .iter()
                .all(|profile| profile.name != "Unsaved draft")
        );
        assert!(profiles.iter().any(|profile|
            profile.name == "Saved draft" && profile.api_key == "saved test key"));
    });

    cx.run_until_parked();
    cx.refresh().unwrap();

    assert!(cancelled.upgrade().is_some());

    cx.update(|window, cx| window.close_dialog(cx));
    cx.run_until_parked();
    cx.refresh().unwrap();
    cx.run_until_parked();

    assert!(cancelled.upgrade().is_none());
}
