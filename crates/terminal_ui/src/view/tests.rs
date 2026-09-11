use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    AppContext, Entity, EntityInputHandler, KeyDownEvent, Keystroke, ListAlignment, Modifiers,
    TestAppContext, VisualTestContext,
};
use nmt_agent::AgentRoute;
use nmt_config::local_state::TabState;

use crate::pane_model::test_session::controller;
use crate::view::list_state::BlockListState;
use crate::view::{AgentInterrupted, PaneIdentity, TerminalPane};
use crate::wake::wake_channel;

fn pane(cx: &mut VisualTestContext) -> Entity<TerminalPane> {
    let (model, _) = controller(b"", true);

    cx.new(|cx| TerminalPane {
        focus: cx.focus_handle(),
        identity: PaneIdentity {
            id: 1,
            profile_name: "Test".into(),
            restorable: TabState::default(),
            agent_route: AgentRoute::parse("test-input").unwrap(),
        },
        model,
        content_bounds: None,
        wake: wake_channel().0,
        image_releases_attached: false,
        block_list: BlockListState::new(ListAlignment::Top),
    })
}

#[gpui::test]
fn accepted_plain_escape_emits_interrupt_but_modified_or_rejected_escape_does_not(
    cx: &mut TestAppContext,
) {
    let cx = cx.add_empty_window();
    let pane = pane(cx);
    let interrupts = Rc::new(Cell::new(0));
    let observed = interrupts.clone();

    let _subscription = cx.update(|_, cx| {
        cx.subscribe(&pane, move |_, _: &AgentInterrupted, _| {
            observed.set(observed.get() + 1);
        })
    });

    for (modifiers, read_only, expected) in [
        (Modifiers::none(), false, 1),
        (Modifiers::shift(), false, 1),
        (
            Modifiers {
                function: true,
                ..Modifiers::none()
            },
            false,
            1,
        ),
        (Modifiers::none(), true, 1),
    ] {
        cx.update(|window, cx| {
            pane.update(cx, |pane, cx| {
                if read_only {
                    pane.model.source.session.mark_read_only();
                }

                pane.on_key_down(
                    &KeyDownEvent {
                        keystroke: Keystroke {
                            modifiers,
                            key: "escape".into(),
                            key_char: None,
                        },
                        is_held: false,
                        prefer_character_input: false,
                    },
                    window,
                    cx,
                );
            })
        });

        assert_eq!(interrupts.get(), expected);
    }
}

#[gpui::test]
fn typing_respects_scroll_setting_for_key_and_ime_input(cx: &mut TestAppContext) {
    let cx = cx.add_empty_window();
    let pane = pane(cx);

    for scroll_when_typing in [false, true] {
        for ime in [false, true] {
            cx.update(|window, cx| {
                pane.update(cx, |pane, cx| {
                    pane.model.settings.scroll_to_bottom_when_typing = scroll_when_typing;
                    pane.model.block_list.scrollbar = (24.0, 120.0);
                    pane.model.update_viewport();

                    if ime {
                        pane.replace_text_in_range(None, "text", window, cx);
                    } else {
                        pane.on_key_down(
                            &KeyDownEvent {
                                keystroke: Keystroke {
                                    modifiers: Modifiers::none(),
                                    key: "enter".into(),
                                    key_char: None,
                                },
                                is_held: false,
                                prefer_character_input: false,
                            },
                            window,
                            cx,
                        );
                    }

                    assert_eq!(pane.model.viewport.is_scrolled(), !scroll_when_typing);
                })
            });
        }
    }

    cx.update(|window, cx| {
        pane.update(cx, |pane, cx| {
            pane.model.source.session.mark_read_only();
            pane.model.block_list.scrollbar = (24.0, 120.0);
            pane.model.update_viewport();
            pane.replace_text_in_range(None, "rejected", window, cx);

            assert!(pane.model.viewport.is_scrolled());
        })
    });
}
