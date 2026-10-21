use gpui::{
    AppContext as _, Context, Entity, IntoElement, ListOffset, ParentElement as _, Render,
    Styled as _, TestAppContext, VisualTestContext, Window, div, px,
};
use nmt_agent::chat::Item;
use nmt_agent::transcript::TextField;
use nmt_config::agent::CollapseRows;

use crate::agent_tab::profile::AgentKind;
use crate::agent_tab::settings::AgentSettings;
use crate::agent_tab::transcript::TranscriptView;

struct TranscriptRoot {
    transcript: Entity<TranscriptView>,
}

impl Render for TranscriptRoot {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().w(px(900.)).h(px(480.)).child(self.transcript.clone())
    }
}

fn draw(cx: &mut VisualTestContext) {
    cx.update(|window, cx| window.draw(cx).clear(cx));
}

#[gpui::test]
fn streaming_while_revisiting_long_reply_preserves_height_and_position(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);

        cx.set_global(AgentSettings {
            reduce_motion: true,
            collapse_tool_calls: CollapseRows::Off,
            ..AgentSettings::default()
        });
    });

    let (root, cx) = cx.add_window_view(|_, cx| TranscriptRoot {
        transcript: cx.new(|cx| {
            let mut view = TranscriptView::new(AgentKind::Codex, None);

            for index in 0..40 {
                let text = if index == 0 {
                    "A paragraph that must remain readable after scrolling away.\n\n".repeat(100)
                } else {
                    format!("Reply {index}\n\n{}", "More content.\n\n".repeat(4))
                };

                view.push(
                    1,
                    Item::AgentMessage {
                        id: format!("reply-{index}"),
                        text: Some(text),
                        questions: None,
                    },
                    Vec::new(),
                    cx,
                );
            }

            view
        }),
    });

    let cx: &mut VisualTestContext = cx;
    let transcript = root.read_with(cx, |root, _| root.transcript.clone());

    cx.run_until_parked();

    transcript.update(cx, |view, cx| {
        view.transcript_list.scroll_to(ListOffset {
            item_ix: 0,
            offset_in_item: px(100.),
        });

        cx.notify();
    });

    cx.run_until_parked();

    draw(cx);

    let original = cx.debug_bounds("transcript-agent-0").unwrap();

    assert!(original.size.height > px(1000.));

    transcript.update(cx, |view, cx| {
        view.scroll_to_bottom();

        cx.notify();
    });

    cx.run_until_parked();

    draw(cx);
    draw(cx);

    assert!(cx.debug_bounds("transcript-agent-0").is_none());

    transcript.update(cx, |view, cx| {
        view.append_delta("reply-39", "Streaming more text.", TextField::Reply);

        view.transcript_list.scroll_to(ListOffset {
            item_ix: 0,
            offset_in_item: px(100.),
        });

        cx.notify();
    });

    draw(cx);

    let returned = cx.debug_bounds("transcript-agent-0").unwrap();

    assert_eq!(returned.size.height, original.size.height);

    transcript.read_with(cx, |view, _| {
        let position = view.transcript_list.logical_scroll_top();

        assert_eq!(position.item_ix, 0);
        assert_eq!(position.offset_in_item, px(100.));
        assert_eq!(view.conversation.borrow().content.entries().len(), 40);
    });
}

#[gpui::test]
fn appending_to_a_visible_reply_preserves_the_reading_position(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);

        cx.set_global(AgentSettings {
            reduce_motion: true,
            collapse_tool_calls: CollapseRows::Off,
            ..AgentSettings::default()
        });
    });

    let (root, cx) = cx.add_window_view(|_, cx| TranscriptRoot {
        transcript: cx.new(|cx| {
            let mut view = TranscriptView::new(AgentKind::Codex, None);

            view.push(
                1,
                Item::AgentMessage {
                    id: "streaming".into(),
                    text: Some("An existing paragraph in a long reply.\n\n".repeat(150)),
                    questions: None,
                },
                Vec::new(),
                cx,
            );

            view
        }),
    });

    let cx: &mut VisualTestContext = cx;
    let transcript = root.read_with(cx, |root, _| root.transcript.clone());

    cx.run_until_parked();

    transcript.update(cx, |view, cx| {
        view.transcript_list.scroll_to(ListOffset {
            item_ix: 0,
            offset_in_item: px(500.),
        });

        cx.notify();
    });

    cx.run_until_parked();

    draw(cx);

    let original = cx.debug_bounds("transcript-agent-0").unwrap();

    for _ in 0..8 {
        transcript.update(cx, |view, cx| {
            view.append_delta("streaming", "A new paragraph.\n\n", TextField::Reply);

            cx.notify();
        });

        cx.run_until_parked();

        draw(cx);

        transcript.read_with(cx, |view, _| {
            assert!(!view.transcript_list.is_following_tail());

            let position = view.transcript_list.logical_scroll_top();

            assert_eq!(position.item_ix, 0);
            assert_eq!(position.offset_in_item, px(500.));
        });

        assert_eq!(
            cx.debug_bounds("transcript-agent-0").unwrap().origin,
            original.origin
        );
    }

    assert!(cx.debug_bounds("transcript-agent-0").unwrap().size.height > original.size.height);
}
