use std::time::Duration;

use gpui::{
    AppContext as _, Context, Entity, IntoElement, ListOffset, Modifiers, MouseButton,
    ParentElement as _, Render, Styled as _, TestAppContext, VisualTestContext, Window, div, point,
    px,
};
use gpui_base::{TextSelection, TextSelectionLayer};
use nmt_agent::chat::Item;
use nmt_agent::transcript::TextField;
use nmt_config::agent::CollapseRows;

use crate::agent_tab::profile::AgentKind;
use crate::agent_tab::settings::AgentSettings;
use crate::agent_tab::transcript::reveal::RevealKey;
use crate::agent_tab::transcript::{Entry, RowSpec, TranscriptView};

struct TranscriptRoot {
    transcript: Entity<TranscriptView>,
}

impl Render for TranscriptRoot {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .w(px(900.))
            .h(px(480.))
            .child(TextSelectionLayer)
            .child(self.transcript.clone())
    }
}

struct UserBubbleRoot {
    transcript: Entity<TranscriptView>,
    prompt: String,
    annotated_prompt: String,
    width: f32,
}

impl Render for UserBubbleRoot {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use crate::agent_tab::transcript::render::user_row::user_prompt_row;

        div()
            .w(px(self.width))
            .children(self.transcript.update(cx, |view, cx| {
                [&self.prompt, &self.annotated_prompt]
                    .into_iter()
                    .enumerate()
                    .map(|(index, text)| {
                        user_prompt_row(view, index, text, Vec::new(), None, |menu, _, _| menu, cx)
                    })
                    .collect::<Vec<_>>()
            }))
    }
}

fn draw(cx: &mut VisualTestContext) {
    cx.update(|window, cx| window.draw(cx).clear(cx));
}

#[gpui::test]
fn expanded_annotations_use_the_user_bubble_width(cx: &mut TestAppContext) {
    use std::slice::from_ref;
    use std::time::Instant;

    use crate::agent_tab::composer::prompt_with_response_annotations;

    cx.update(|cx| {
        gpui_component::init(cx);

        cx.set_global(AgentSettings {
            reduce_motion: true,
            collapse_tool_calls: CollapseRows::Off,
            ..AgentSettings::default()
        });
    });

    let long_text = "Long annotation text with words that should wrap. ".repeat(12);

    let (root, cx) = cx.add_window_view(|_, cx| UserBubbleRoot {
        width: 900.,
        annotated_prompt: prompt_with_response_annotations("Explain this", from_ref(&long_text)),
        prompt: long_text,
        transcript: cx.new(|_| {
            let mut view = TranscriptView::new(AgentKind::Claude, None);

            view.disclosures
                .open(RevealKey::Annotation(1), Instant::now(), false);

            view
        }),
    });

    let cx: &mut VisualTestContext = cx;

    for width in [900., 420.] {
        root.update(cx, |root, cx| {
            root.width = width;

            cx.notify();
        });

        cx.run_until_parked();

        draw(cx);
        draw(cx);

        let annotation = cx.debug_bounds("transcript-annotations-1").unwrap();
        let message = cx.debug_bounds("transcript-user-0").unwrap();

        assert!(message.size.width > px(width * 0.6));
        assert!(
            (annotation.size.width - message.size.width).abs() <= px(1.),
            "annotation: {annotation:?}, message: {message:?}"
        );
        assert!((annotation.right() - message.right()).abs() <= px(1.));
        assert!(annotation.left() >= px(0.));
    }
}

#[gpui::test]
fn horizontal_text_selection_preserves_transcript_scroll_position(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);

        cx.set_global(AgentSettings {
            reduce_motion: true,
            collapse_tool_calls: CollapseRows::Off,
            ..AgentSettings::default()
        });
    });

    let (root, cx) = cx.add_window_view(|_, cx| TranscriptRoot {
        transcript: cx.new(|_| {
            let mut view = TranscriptView::new(AgentKind::Claude, None);

            for index in 0..40 {
                view.push_stamped(
                    1,
                    Item::AgentMessage {
                        id: format!("reply-{index}"),
                        text: Some("Selectable words in a completed reply.".into()),
                        questions: None,
                    },
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
            item_ix: 20,
            offset_in_item: px(0.),
        });

        cx.notify();
    });

    cx.run_until_parked();

    draw(cx);
    draw(cx);

    for selector in [
        "transcript-agent-20",
        "transcript-agent-23",
        "transcript-agent-28",
    ] {
        let bounds = cx.debug_bounds(selector).expect("a visible reply");
        let start = point(bounds.left() + px(8.), bounds.top() + px(8.));
        let end = point(start.x + px(100.), start.y);
        let before = transcript.read_with(cx, |view, _| view.transcript_list.logical_scroll_top());

        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());

        // The first draw updates selection; the next draw uses the cached row.
        draw(cx);
        draw(cx);

        cx.simulate_mouse_move(end, Some(MouseButton::Left), Modifiers::default());
        cx.run_until_parked();

        for _ in 0..6 {
            cx.executor().advance_clock(Duration::from_millis(20));
            cx.run_until_parked();

            draw(cx);
        }

        let after = transcript.read_with(cx, |view, _| view.transcript_list.logical_scroll_top());

        cx.simulate_mouse_up(end, MouseButton::Left, Modifiers::default());

        assert!(
            !cx.update(TextSelection::selected_text).is_empty(),
            "{selector}"
        );
        assert_eq!(
            (after.item_ix, after.offset_in_item),
            (before.item_ix, before.offset_in_item),
            "horizontal selection moved the transcript at {selector}"
        );
    }

    for end_y in [-10., 490.] {
        transcript.update(cx, |view, cx| {
            view.transcript_list.scroll_to(ListOffset {
                item_ix: 20,
                offset_in_item: px(0.),
            });

            cx.notify();
        });

        draw(cx);
        draw(cx);

        let bounds = cx.debug_bounds("transcript-agent-23").unwrap();
        let start = point(bounds.left() + px(8.), bounds.top() + px(8.));
        let end = point(start.x, px(end_y));

        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());

        draw(cx);
        draw(cx);

        cx.simulate_mouse_move(end, Some(MouseButton::Left), Modifiers::default());

        for _ in 0..6 {
            cx.executor().advance_clock(Duration::from_millis(20));
            cx.run_until_parked();

            draw(cx);
        }

        let after = transcript.read_with(cx, |view, _| view.transcript_list.logical_scroll_top());

        cx.simulate_mouse_up(end, MouseButton::Left, Modifiers::default());

        if end_y < 0. {
            assert!(
                after.item_ix < 20,
                "dragging above the viewport must scroll up"
            );
        } else {
            assert!(
                after.item_ix > 20 || after.offset_in_item > px(0.),
                "dragging below the viewport must scroll down"
            );
        }
    }
}

#[gpui::test]
fn retracting_a_reply_before_its_text_update_is_drawn_clears_the_row(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);

        cx.set_global(AgentSettings::default());
    });

    let (root, cx) = cx.add_window_view(|_, cx| TranscriptRoot {
        transcript: cx.new(|_| {
            let mut view = TranscriptView::new(AgentKind::Claude, None);

            view.push_stamped(
                1,
                Item::AgentMessage {
                    id: "refused".into(),
                    text: Some("Partial reply".into()),
                    questions: None,
                },
            );

            view
        }),
    });

    let cx: &mut VisualTestContext = cx;
    let transcript = root.read_with(cx, |root, _| root.transcript.clone());

    cx.run_until_parked();

    draw(cx);

    assert!(cx.debug_bounds("transcript-agent-0").is_some());

    transcript.update(cx, |view, cx| {
        let mut conversation = view.conversation.borrow_mut();

        conversation.append_delta("refused", " unread tail", TextField::Reply);
        conversation.remove_items(&["refused".into()]);

        drop(conversation);

        view.sync_content();

        cx.notify();
    });

    cx.run_until_parked();

    draw(cx);

    assert!(cx.debug_bounds("transcript-agent-0").is_none());

    transcript.read_with(cx, |view, _| assert!(view.rows.is_empty()));
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

    transcript.read_with(cx, |view, _| {
        assert_eq!(view.text_views.fragments(0).len(), 100);
        assert!(
            view.rows
                .iter()
                .filter(|row| matches!(row.spec, RowSpec::Entry { index: 0, .. }))
                .all(|row| row.block.is_some())
        );
    });

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
            item_ix: 60,
            offset_in_item: px(5.),
        });

        cx.notify();
    });

    cx.run_until_parked();

    draw(cx);

    let original = cx.debug_bounds("transcript-agent-0").unwrap();

    let original_blocks = transcript.read_with(cx, |view, _| view.text_views.fragments(0).len());

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

            assert_eq!(position.item_ix, 60);
            assert_eq!(position.offset_in_item, px(5.));
        });

        assert_eq!(
            cx.debug_bounds("transcript-agent-0").unwrap().origin,
            original.origin
        );
    }

    transcript.read_with(cx, |view, _| {
        assert_eq!(view.text_views.fragments(0).len(), original_blocks + 8);
    });
}

#[gpui::test]
fn thousand_block_reply_only_constructs_visible_views(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);

        cx.set_global(AgentSettings {
            reduce_motion: true,
            ..AgentSettings::default()
        });
    });

    let (root, cx) = cx.add_window_view(|_, cx| TranscriptRoot {
        transcript: cx.new(|cx| {
            let mut view = TranscriptView::new(AgentKind::Codex, None);

            view.push(
                1,
                Item::AgentMessage {
                    id: "long".into(),
                    text: Some("A paragraph with enough text to measure.\n\n".repeat(1000)),
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

    draw(cx);
    draw(cx);

    let original = transcript.read_with(cx, |view, _| {
        assert_eq!(view.text_views.fragments(0).len(), 1000);

        let ids = view.text_views.fragment_ids(0);

        assert!(!ids.is_empty());
        assert!(ids.len() <= 64, "{}", ids.len());
        assert!(view.text_views.fragment_creations(0) <= 64);

        ids
    });

    draw(cx);
    draw(cx);

    transcript.read_with(cx, |view, _| {
        assert_eq!(view.text_views.fragment_ids(0), original)
    });

    for index in [0, 250, 500, 750, 999, 0] {
        transcript.update(cx, |view, cx| {
            view.transcript_list.scroll_to(ListOffset {
                item_ix: index,
                offset_in_item: px(0.),
            });

            cx.notify();
        });

        draw(cx);
        draw(cx);

        transcript.read_with(cx, |view, _| {
            assert!(view.text_views.fragment_ids(0).len() <= 64)
        });
    }

    transcript.read_with(cx, |view, _| {
        assert!(view.text_views.fragment_creations(0) < 400)
    });

    transcript.update(cx, |view, cx| {
        view.push_stamped(
            2,
            Item::AgentMessage {
                id: "temporary".into(),
                text: Some("A temporary reply.".into()),
                questions: None,
            },
        );

        cx.notify();
    });

    draw(cx);

    transcript.update(cx, |view, cx| {
        let item = view.conversation.borrow().content.entries()[0].item.clone();

        let entries = vec![Entry {
            turn: 1,
            item,
            metadata: Default::default(),
        }];

        view.show_attributed_entries(entries, Default::default(), 1, cx);
    });

    draw(cx);

    transcript.read_with(cx, |view, _| {
        assert_eq!(view.text_views.fragments(0).len(), 1000)
    });
}

#[gpui::test]
fn appending_a_second_block_preserves_the_reading_position(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);

        cx.set_global(AgentSettings {
            reduce_motion: true,
            ..AgentSettings::default()
        });
    });

    let (root, cx) = cx.add_window_view(|_, cx| TranscriptRoot {
        transcript: cx.new(|cx| {
            let mut view = TranscriptView::new(AgentKind::Codex, None);

            view.push(
                1,
                Item::AgentMessage {
                    id: "threshold".into(),
                    text: Some(format!("{}\n\n", "An existing paragraph. ".repeat(100))),
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

    draw(cx);

    transcript.update(cx, |view, cx| {
        assert_eq!(view.text_views.fragments(0).len(), 1);

        view.transcript_list.scroll_to(ListOffset {
            item_ix: 0,
            offset_in_item: px(100.),
        });

        cx.notify();
    });

    draw(cx);

    let top = transcript.read_with(cx, |view, _| {
        view.transcript_list.bounds_for_item(0).unwrap().top()
    });

    transcript.update(cx, |view, cx| {
        view.append_delta("threshold", "An appended paragraph.\n\n", TextField::Reply);

        cx.notify();
    });

    cx.run_until_parked();

    draw(cx);
    draw(cx);

    transcript.read_with(cx, |view, _| {
        assert_eq!(view.text_views.fragments(0).len(), 2);
        assert_eq!(view.transcript_list.logical_scroll_top().item_ix, 0);
        assert_eq!(
            view.transcript_list.logical_scroll_top().offset_in_item,
            px(100.)
        );
        assert_eq!(view.transcript_list.bounds_for_item(0).unwrap().top(), top);
    });
}

#[gpui::test]
fn mermaid_extension_is_configured_before_reply_fragmentation(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);

        cx.set_global(AgentSettings {
            reduce_motion: true,
            ..AgentSettings::default()
        });
    });

    let (root, cx) = cx.add_window_view(|_, cx| TranscriptRoot {
        transcript: cx.new(|cx| {
            let mut view = TranscriptView::new(AgentKind::Codex, None);

            let text = format!(
                "```mermaid\nflowchart TD\nA --> B\n```\n\n{}",
                "Another paragraph.\n\n".repeat(16)
            );

            view.push(
                1,
                Item::AgentMessage {
                    id: "diagram".into(),
                    text: Some(text),
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

    draw(cx);

    transcript.update(cx, |view, cx| {
        view.transcript_list.scroll_to(ListOffset {
            item_ix: 0,
            offset_in_item: px(0.),
        });

        cx.notify();
    });

    for _ in 0..3 {
        cx.run_until_parked();

        draw(cx);
    }

    transcript.read_with(cx, |view, _| {
        assert_eq!(view.text_views.fragments(0).len(), 17)
    });

    assert!(cx.debug_bounds("mermaid-preview").is_some());
}

#[gpui::test]
fn thinking_setting_updates_existing_rows_and_keeps_the_full_message_open(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);

        cx.set_global(AgentSettings {
            reduce_motion: true,
            collapse_tool_calls: CollapseRows::ToolCalls,
            ..AgentSettings::default()
        });
    });

    let (root, cx) = cx.add_window_view(|_, cx| {
        cx.observe_global::<AgentSettings>(|_, cx| cx.notify())
            .detach();

        TranscriptRoot {
            transcript: cx.new(|_| {
                let mut view = TranscriptView::new(AgentKind::Codex, None);

                for index in 0..5 {
                    let item = if index == 2 {
                        Item::Reasoning {
                            id: "reasoning".into(),
                            summary: Some("Reviewing the result.".into()),
                        }
                    } else {
                        Item::Other {
                            id: format!("tool-{index}"),
                            kind: "tool".into(),
                            title: "Example tool".into(),
                            output: Some("done".into()),
                            status: Some("completed".into()),
                        }
                    };

                    view.push_stamped(1, item);
                }

                view
            }),
        }
    });

    let cx: &mut VisualTestContext = cx;
    let transcript = root.read_with(cx, |root, _| root.transcript.clone());

    cx.run_until_parked();

    draw(cx);

    assert!(cx.debug_bounds("thinking-body-2").is_none());

    transcript.read_with(cx, |view, _| {
        assert_eq!(view.rows.len(), 1);
        assert!(matches!(
            view.rows[0].spec,
            RowSpec::RunToggle { tool_count: 5, .. }
        ));
    });

    cx.update_global::<AgentSettings, _>(|settings, _| settings.show_thinking_in_stream = true);
    cx.run_until_parked();

    draw(cx);

    transcript.read_with(cx, |view, _| {
        assert_eq!(view.rows.len(), 3);
        assert!(matches!(view.rows[1].spec, RowSpec::Entry { index: 2, .. }));
    });

    let body = cx.debug_bounds("thinking-body-2").unwrap();
    let header = cx.debug_bounds("thinking-head-2").unwrap();

    assert!(body.size.height > px(0.));

    cx.simulate_click(header.center(), Default::default());
    cx.run_until_parked();

    draw(cx);

    assert_eq!(cx.debug_bounds("thinking-body-2").unwrap(), body);

    transcript.read_with(cx, |view, _| {
        assert!(!view.disclosures.is_disclosing(RevealKey::Row(2)));
    });

    transcript.update(cx, |view, cx| {
        view.append_delta(
            "reasoning",
            &"\n\nMore reasoning that remains visible in the message stream.".repeat(30),
            TextField::ReasoningSummary,
        );

        cx.notify();
    });

    cx.run_until_parked();

    draw(cx);

    assert!(cx.debug_bounds("thinking-body-2").unwrap().size.height > px(256.));

    cx.update_global::<AgentSettings, _>(|settings, _| settings.show_thinking_in_stream = false);
    cx.run_until_parked();

    draw(cx);

    assert!(cx.debug_bounds("thinking-body-2").is_none());

    transcript.read_with(cx, |view, _| {
        assert_eq!(view.rows.len(), 1);
        assert!(matches!(
            view.rows[0].spec,
            RowSpec::RunToggle { tool_count: 5, .. }
        ));
    });
}
