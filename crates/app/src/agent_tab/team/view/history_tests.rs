use std::path::Path;

use gpui::{AppContext as _, TestAppContext, VisualTestContext};
use gpui_component::Root;
use nmt_agent::AgentWorkspace;
use nmt_agent::chat::ThreadSettings;
use nmt_agent::session::AgentKind;
use nmt_agent::team::member::{MemberConfig, ProfileReference};
use nmt_agent::team::room::Room;
use nmt_agent::team::storage::RoomStore;
use tempfile::tempdir;

use crate::agent_tab::settings::AgentSettings;
use crate::agent_tab::team::{TeamPane, TeamRuntime};
use crate::agent_tab::view::recent_sessions::RecentSessionsMode;

fn saved_room(directory: &Path, cwd: &str, name: &str) -> RoomStore {
    let workspace = AgentWorkspace::single(Some(cwd.into()));

    let mut room = Room::new(workspace.clone());

    room.add_member(MemberConfig {
        name: name.into(),
        profile: ProfileReference {
            kind: AgentKind::Codex,
            name: "unconfigured".into(),
        },
        roots: workspace,
        settings: ThreadSettings::default(),
        role: String::new(),
    })
    .unwrap();

    RoomStore::create(directory, room).unwrap()
}

#[gpui::test]
async fn history_scopes_rooms_and_preserves_current_room_when_reopen_fails(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();

    let directory = tempdir().unwrap();
    let saved = saved_room(directory.path(), "C:/project", "Alice");
    let target = saved.room().id();
    let other = saved_room(directory.path(), "C:/other", "Bob");

    let (runtime, pane, window) = cx.update(|cx| {
        gpui_component::init(cx);

        cx.set_global(AgentSettings::default());

        let runtime = TeamRuntime::create(
            directory.path(),
            AgentWorkspace::single(Some("C:/project".into())),
            cx,
        );

        let mut pane = None;

        let window = cx
            .open_window(Default::default(), |window, cx| {
                let view = cx.new(|cx| TeamPane::new(runtime.clone(), window, cx));

                pane = Some(view.clone());

                cx.new(|cx| Root::new(view, window, cx))
            })
            .unwrap();

        (runtime, pane.unwrap(), window)
    });

    cx.condition(&runtime, |runtime, _| !runtime.loading())
        .await;

    let mut cx = VisualTestContext::from_window(window.into(), cx);

    cx.condition(&pane, |pane, cx| !pane.history.rows(pane, cx).is_empty())
        .await;

    pane.update_in(&mut cx, |pane, window, cx| {
        assert!(pane.history.visible(pane, cx));
        assert_eq!(
            pane.history
                .rows(pane, cx)
                .iter()
                .map(|row| row.id)
                .collect::<Vec<_>>(),
            vec![target]
        );

        pane.history.toggle_scope(cx);

        assert_eq!(pane.history.rows(pane, cx).len(), 2);

        pane.history.toggle_scope(cx);

        pane.input
            .update(cx, |input, cx| input.set_value("Draft to keep", window, cx));

        assert!(!pane.history.visible(pane, cx));

        pane.toggle_history(cx);

        assert!(pane.history.visible(pane, cx));

        pane.resume_room(target, window, cx);

        assert!(pane.history.pending.is_some());
    });

    cx.condition(&pane, |pane, _| pane.history.pending.is_none())
        .await;

    pane.update(&mut cx, |pane, cx| {
        assert_eq!(pane.room_id(cx), runtime.read(cx).id());
        assert_eq!(pane.input.read(cx).text().to_string(), "Draft to keep");
        assert!(pane.error.is_some());
    });

    drop(saved);
    drop(other);

    pane.update_in(&mut cx, |pane, window, cx| {
        pane.resume_room(target, window, cx);

        assert!(pane.history.pending.is_some());
    });

    cx.condition(&pane, |pane, cx| pane.room_id(cx) == target)
        .await;

    pane.update(&mut cx, |pane, cx| {
        assert_eq!(pane.runtime.read(cx).room().members()[0].name(), "Alice");
        assert_eq!(
            pane.runtime.read(cx).room().workspace().primary(),
            Some("C:/project")
        );
        assert_eq!(pane.history.mode, RecentSessionsMode::Hidden);
        assert_eq!(pane.input.read(cx).text().len(), 0);
        assert!(pane.error.is_none());
    });
}
