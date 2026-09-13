use std::cell::RefCell;
use std::fs;
use std::io::Cursor;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use gpui::{AppContext as _, Image, ImageFormat, TestAppContext, VisualTestContext};
use gpui_component::Root;
use image_rs::{DynamicImage, ImageFormat as EncodedImageFormat, RgbaImage};
use nmt_agent::chat::{Event, Item, SendOutcome, SlashCommandOutcome, ThreadSettings};
use nmt_agent::session::Backend;
use nmt_agent::session::test_support::TestBackend;
use nmt_agent::{AgentEventKind, AgentWorkspace};
use nmt_config::profile::{AgentProfile, AgentProfileKind};

use crate::agent_tab::composer::attachments::scratch_dir;
use crate::agent_tab::execution::{AgentSession, SessionRegistry};
use crate::agent_tab::settings::AgentSettings;
use crate::agent_tab::transcript::TranscriptView;
use crate::agent_tab::{
    AgentKind, AgentPane, AgentPaneEvent, AgentThreadDefaults, RecoveryReadiness,
};

#[gpui::test]
async fn detached_session_retains_output_and_interaction_until_owner_close(
    cx: &mut TestAppContext,
) {
    let (owner, window) = cx.update(|cx| {
        gpui_component::init(cx);
        cx.set_global(AgentSettings::default());
        cx.set_global(AgentThreadDefaults::default());

        let owner = AgentSession::create(
            AgentProfile {
                kind: AgentProfileKind::Codex,
                ..AgentProfile::default()
            },
            AgentWorkspace::default(),
            None,
            cx,
        );

        let window = cx
            .open_window(Default::default(), |window, cx| {
                let view = cx.new(|_| TranscriptView::new(AgentKind::Codex, None));

                cx.new(|cx| Root::new(view, window, cx))
            })
            .unwrap();

        (owner, window)
    });

    let host = owner.session().clone();
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    let released = Arc::new(AtomicBool::new(false));
    let events = Rc::new(RefCell::new(Vec::new()));
    let observed = events.clone();

    let subscription = cx.update(|_, cx| {
        cx.subscribe(&host, move |_, event: &AgentPaneEvent, _| {
            observed.borrow_mut().push(event.clone())
        })
    });

    cx.update(|_, cx| assert_eq!(SessionRegistry::sessions(cx).len(), 1));

    let epoch = host.update(&mut cx, |session, cx| {
        let mut backend = TestBackend::new(
            [SendOutcome::StartedTurn],
            SlashCommandOutcome::NotReady,
            vec![],
        )
        .with_recovery(AgentKind::Codex, "retained-thread")
        .watch_release(released.clone());

        backend.approval_accepted = true;

        let epoch = session.controller.borrow_mut().starting(None).epoch;

        assert_eq!(
            session.install(Ok(Backend::Test(backend)), epoch, "test", cx),
            Some(true)
        );

        session.on_event(epoch, Event::Ready(ThreadSettings::default()), cx);

        epoch
    });

    let first = cx.update(|window, cx| cx.new(|cx| AgentPane::attach(&owner, window, cx)));
    let stale = cx.update(|window, cx| cx.new(|cx| AgentPane::attach(&owner, window, cx)));

    cx.update(|_, cx| assert!(!first.read(cx).binding.is_current()));

    first.update(&mut cx, |pane, cx| {
        assert!(!pane.send_text_inner("obsolete".into(), None, None, cx))
    });

    let mut bytes = Cursor::new(Vec::new());

    DynamicImage::ImageRgba8(RgbaImage::new(1, 1))
        .write_to(&mut bytes, EncodedImageFormat::Png)
        .unwrap();

    let image = Image::from_bytes(ImageFormat::Png, bytes.into_inner());

    let (weak, scratch) = cx.update(|window, cx| {
        stale.update(cx, |pane, cx| {
            pane.attachments
                .attach_image(&image, &pane.input, window, cx)
                .ok()
                .expect("attach test image");

            assert!(pane.send_text_inner("accepted image".into(), None, None, cx));

            let state = pane.session.borrow();
            let conversation = state.conversation.borrow();
            let image = &conversation.content.entries()[0].metadata.images[0];
            let scratch = scratch_dir(pane.agent_route.as_str());

            fs::create_dir_all(&scratch).unwrap();
            fs::write(scratch.join("delayed-read.png"), &image.bytes).unwrap();

            (Arc::downgrade(image), scratch)
        })
    });

    drop(first);
    drop(stale);
    cx.run_until_parked();

    host.update(&mut cx, |session, cx| {
        session.on_event(epoch, Event::TurnStarted, cx);

        session.on_event(
            epoch,
            Event::ItemStarted(Item::AgentMessage {
                id: "reply".into(),
                text: Some("kept".into()),
                questions: None,
            }),
            cx,
        );

        session.on_event(
            epoch,
            Event::AgentMessageDelta {
                item_id: "reply".into(),
                delta: " without a view".into(),
            },
            cx,
        );

        session.on_event(
            epoch,
            Event::ApprovalRequested {
                description: "Approve retained operation".into(),
            },
            cx,
        );
    });

    assert!(weak.upgrade().is_some());
    assert!(
        !fs::read(scratch.join("delayed-read.png"))
            .unwrap()
            .is_empty()
    );

    let second = cx.update(|window, cx| cx.new(|cx| AgentPane::attach(&owner, window, cx)));

    second.update(&mut cx, |pane, cx| {
        assert_eq!(pane.session.borrow().runtime.epoch(), epoch);
        assert_eq!(
            pane.session
                .borrow()
                .runtime
                .backend()
                .unwrap()
                .recovery_identity()
                .unwrap()
                .id,
            "retained-thread"
        );
        assert_eq!(
            pane.latest_agent_message(cx).as_deref(),
            Some("kept without a view")
        );

        pane.respond_approval("accept", cx);
        pane.respond_approval("accept", cx);

        let state = pane.session.borrow();

        let Some(Backend::Test(backend)) = state.runtime.backend() else {
            panic!("test backend");
        };

        assert_eq!(backend.approval_responses, ["accept"]);
    });

    host.update(&mut cx, |session, cx| {
        session.on_event(epoch, Event::TurnCompleted { error: None }, cx);
        session.on_event(epoch, Event::TurnCompleted { error: None }, cx);
    });

    cx.run_until_parked();

    assert_eq!(events.borrow().iter().filter(|event| matches!(event, AgentPaneEvent::Lifecycle(event) if event.kind == AgentEventKind::Stopped)).count(), 1);

    let recovery = cx.update(|_, cx| match host.read(cx).recovery_readiness(cx) {
        RecoveryReadiness::Ready(snapshot) => snapshot,
        _ => panic!("settled retained session is recoverable"),
    });

    owner.close();

    host.update(&mut cx, |session, cx| {
        session.restore_after_update(&recovery, cx)
    });

    assert!(!owner.bind().is_current());

    cx.update(|_, cx| assert!(SessionRegistry::sessions(cx).is_empty()));

    assert!(weak.upgrade().is_none());

    second.update(&mut cx, |pane, cx| {
        assert!(!pane.send_text_inner("closed".into(), None, None, cx))
    });

    host.update(&mut cx, |session, cx| {
        session.on_event(
            epoch,
            Event::AgentMessageDelta {
                item_id: "reply".into(),
                delta: "late".into(),
            },
            cx,
        )
    });

    cx.update(|_, cx| {
        assert!(
            host.read(cx)
                .controller
                .borrow()
                .conversation
                .borrow()
                .content
                .entries()
                .is_empty()
        )
    });

    for _ in 0..100 {
        if released.load(Ordering::SeqCst) && !scratch.exists() {
            break;
        }

        cx.background_executor.timer(Duration::from_millis(1)).await;
    }

    assert!(released.load(Ordering::SeqCst));
    assert!(!scratch.exists());

    drop(subscription);
}
