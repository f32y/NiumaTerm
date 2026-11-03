use std::time::Duration;

use gpui::{
    AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, Styled as _,
    TestAppContext, VisualTestContext, Window, div, px,
};
use gpui_base::text::search::LiteralSearch;
use nmt_agent::chat::Item;
use nmt_config::agent::CollapseRows;

use crate::agent_tab::profile::AgentKind;
use crate::agent_tab::settings::AgentSettings;
use crate::agent_tab::transcript::TranscriptView;
use crate::agent_tab::transcript::reveal::RevealKey;
use crate::agent_tab::transcript::search::{SearchPart, collect_sections, handle_event};
use crate::agent_tab::transcript::text::TextPart;
use crate::find::FindEvent;

struct SearchRoot {
    transcript: Entity<TranscriptView>,
}

impl Render for SearchRoot {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().w(px(900.)).h(px(480.)).child(self.transcript.clone())
    }
}

fn draw(cx: &mut VisualTestContext) {
    cx.run_until_parked();

    cx.update(|window, cx| {
        window.simulate_next_frame(cx);
        window.draw(cx).clear(cx);
    });

    cx.run_until_parked();
}

#[test]
fn agent_search_matches_rendered_reply_and_normalized_tool_output() {
    let query = LiteralSearch::new("needle");

    let mut sections = Vec::new();

    collect_sections(
        0,
        &Item::AgentMessage {
            id: "a".into(),
            text: Some("**Needle** [label](https://needle.example)".into()),
            questions: None,
        },
        &query,
        &mut sections,
    );

    collect_sections(
        1,
        &Item::CommandExecution {
            id: "b".into(),
            command: "echo needle".into(),
            purpose: None,
            aggregated_output: Some("\x1b[31mNEEDLE\x1b[0m".into()),
            status: None,
            exit_code: None,
        },
        &query,
        &mut sections,
    );

    assert_eq!(sections.len(), 2);
    assert_eq!(sections[0].matches.len(), 1);
    assert_eq!(sections[1].matches.len(), 2);
    assert!(sections[0].part == SearchPart::Text(TextPart::Reply));
    assert!(sections[1].part == SearchPart::Code);
    assert_eq!(sections[1].matches[1].range, 15..21);
}

#[gpui::test]
fn agent_search_reveals_folded_history_and_keeps_position_on_new_output(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);

        cx.set_global(AgentSettings {
            reduce_motion: true,
            collapse_tool_calls: CollapseRows::Off,
            ..Default::default()
        });
    });

    let (root, cx) = cx.add_window_view(|_, cx| SearchRoot {
        transcript: cx.new(|cx| {
            let mut view = TranscriptView::new(AgentKind::Codex, None);

            for index in 0..30 {
                view.push(
                    1,
                    Item::AgentMessage {
                        id: format!("reply-{index}"),
                        text: Some(format!(
                            "Paragraph {index}. {}",
                            "History text. ".repeat(30)
                        )),
                        questions: None,
                    },
                    Vec::new(),
                    cx,
                );
            }

            view.push(
                1,
                Item::CommandExecution {
                    id: "tool".into(),
                    command: "echo needle".into(),
                    purpose: None,
                    aggregated_output: Some("needle output".into()),
                    status: Some("completed".into()),
                    exit_code: Some(0),
                },
                Vec::new(),
                cx,
            );

            view
        }),
    });

    let transcript = root.read_with(cx, |root, _| root.transcript.clone());

    draw(cx);

    cx.update(|window, cx| transcript.update(cx, |view, cx| view.open_find(window, cx)));

    draw(cx);

    transcript.update(cx, |view, cx| {
        handle_event(view, &FindEvent::Query("needle".into()), cx)
    });

    for _ in 0..6 {
        draw(cx);
    }

    transcript.read_with(cx, |view, _| {
        assert_eq!(view.search.hits.len(), 2);
        assert!(view.disclosures.row_expanded(30));
        assert!(!view.transcript_list.is_following_tail());
    });

    let before = transcript.read_with(cx, |view, _| {
        assert!(!view.search.pending);

        let bounds = view.search.bounds.lock().unwrap();

        assert!(
            view.transcript_list
                .viewport_bounds()
                .contains(&bounds.origin)
        );

        (
            view.search.hits[view.search.current.unwrap()].clone(),
            bounds,
        )
    });

    transcript.update(cx, |view, cx| {
        view.push(
            2,
            Item::AgentMessage {
                id: "new".into(),
                text: Some("new needle".into()),
                questions: None,
            },
            Vec::new(),
            cx,
        );
    });

    for _ in 0..4 {
        draw(cx);
    }

    transcript.read_with(cx, |view, _| {
        assert_eq!(view.search.hits.len(), 3);
        assert!(view.search.hits[view.search.current.unwrap()] == before.0);
        assert_eq!(view.search.bounds.lock().unwrap(), before.1);
    });

    transcript.update(cx, |view, cx| handle_event(view, &FindEvent::Close, cx));
    transcript.read_with(cx, |view, _| assert!(!view.disclosures.row_expanded(30)));
}

#[gpui::test]
fn agent_search_discards_previous_query_and_preserves_manual_expansion(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);

        cx.set_global(AgentSettings {
            reduce_motion: true,
            ..Default::default()
        });
    });

    let (root, cx) = cx.add_window_view(|_, cx| SearchRoot {
        transcript: cx.new(|cx| {
            let mut view = TranscriptView::new(AgentKind::Codex, None);

            view.push(
                1,
                Item::UserMessage {
                    text: Some(format!("{} needle", "message ".repeat(1000))),
                },
                Vec::new(),
                cx,
            );

            view
        }),
    });

    let transcript = root.read_with(cx, |root, _| root.transcript.clone());

    cx.update(|window, cx| transcript.update(cx, |view, cx| view.open_find(window, cx)));

    draw(cx);

    transcript.update(cx, |view, cx| {
        handle_event(view, &FindEvent::Query("needle".into()), cx);
        handle_event(view, &FindEvent::Editing, cx);
        handle_event(view, &FindEvent::Query("missing".into()), cx);
    });

    for _ in 0..3 {
        draw(cx);
    }

    transcript.read_with(cx, |view, _| assert!(view.search.hits.is_empty()));

    transcript.update(cx, |view, cx| {
        handle_event(view, &FindEvent::Query("needle".into()), cx)
    });

    for _ in 0..4 {
        draw(cx);
    }

    transcript.update(cx, |view, cx| {
        assert_eq!(view.search.hits.len(), 1);
        assert!(view.disclosures.row_expanded(0));

        view.toggle_disclosure(RevealKey::Row(0), cx);
        view.toggle_disclosure(RevealKey::Row(0), cx);

        handle_event(view, &FindEvent::Close, cx);

        assert!(view.disclosures.row_expanded(0));
    });

    cx.executor().advance_clock(Duration::from_millis(120));
}

#[gpui::test]
fn agent_search_navigates_large_code_output_and_wraps(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);

        cx.set_global(AgentSettings {
            reduce_motion: true,
            collapse_tool_calls: CollapseRows::Off,
            ..Default::default()
        });
    });

    let (root, cx) = cx.add_window_view(|_, cx| SearchRoot {
        transcript: cx.new(|cx| {
            let mut view = TranscriptView::new(AgentKind::Codex, None);

            let output: String = (0..600)
                .map(|index| {
                    format!(
                        "line {index} {}\n",
                        if matches!(index, 50 | 500) {
                            "needle"
                        } else {
                            "content"
                        }
                    )
                })
                .collect();

            view.push(
                1,
                Item::CommandExecution {
                    id: "tool".into(),
                    command: "print output".into(),
                    purpose: None,
                    aggregated_output: Some(output),
                    status: Some("completed".into()),
                    exit_code: Some(0),
                },
                Vec::new(),
                cx,
            );

            view.push(
                1,
                Item::AgentMessage {
                    id: "reply".into(),
                    text: Some("Finished.\n\n".repeat(100)),
                    questions: None,
                },
                Vec::new(),
                cx,
            );

            view
        }),
    });

    let transcript = root.read_with(cx, |root, _| root.transcript.clone());

    draw(cx);

    cx.update(|window, cx| transcript.update(cx, |view, cx| view.open_find(window, cx)));

    transcript.update(cx, |view, cx| {
        handle_event(view, &FindEvent::Query("needle".into()), cx)
    });

    for expected in [0, 1, 0] {
        for _ in 0..8 {
            draw(cx);
        }

        transcript.read_with(cx, |view, _| {
            assert_eq!(view.search.hits.len(), 2);
            assert_eq!(view.search.current, Some(expected));
            assert!(!view.search.pending);

            let bounds = view.search.bounds.lock().unwrap();
            let viewport = view.transcript_list.viewport_bounds();

            assert!(
                bounds.top() >= viewport.top() && bounds.bottom() <= viewport.bottom(),
                "match {bounds:?}, viewport {viewport:?}"
            );
        });

        transcript.update(cx, |view, cx| {
            handle_event(view, &FindEvent::Navigate { backwards: false }, cx)
        });
    }
}
