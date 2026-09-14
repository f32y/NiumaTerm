use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    AppContext, Bounds, Entity, EntityInputHandler, KeyDownEvent, KeyUpEvent, Keystroke,
    ListAlignment, Modifiers, TestAppContext, VisualTestContext, point, px, size,
};
use nmt_agent::AgentRoute;
use nmt_config::local_state::TabState;

use crate::terminal_tab::metrics::CellMetrics;
use crate::terminal_tab::pane_model::test_session::{assert_input, controller};
use crate::terminal_tab::view::list_state::BlockListState;
use crate::terminal_tab::view::{
    AgentInterrupted, PaneIdentity, TerminalGridResized, TerminalPane,
};
use crate::terminal_tab::wake::wake_channel;

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
fn layout_saves_the_accepted_grid_and_emits_only_on_resize(cx: &mut TestAppContext) {
    let cx = cx.add_empty_window();
    let pane = pane(cx);
    let changes = Rc::new(Cell::new(0));
    let observed = changes.clone();

    cx.update(|_, cx| {
        cx.subscribe(&pane, move |_, _: &TerminalGridResized, _| {
            observed.set(observed.get() + 1);
        })
        .detach();
    });

    for (cols, rows, expected_changes) in [(40, 6, 0), (132, 43, 1), (132, 43, 1)] {
        cx.update(|_, cx| {
            pane.update(cx, |pane, cx| {
                pane.set_content_bounds(
                    Bounds {
                        origin: point(px(0.0), px(0.0)),
                        size: size(px(cols as f32 * 8.0), px(rows as f32 * 18.0)),
                    },
                    CellMetrics {
                        width_px: 8.0,
                        height_px: 18.0,
                    },
                    cx,
                );

                assert_eq!(pane.tab_state().grid_size, Some((cols, rows)));
            });
        });

        cx.run_until_parked();

        assert_eq!(changes.get(), expected_changes);
    }
}

#[cfg(windows)]
#[gpui::test]
fn restored_grid_reaches_the_shell_before_its_first_output(cx: &mut TestAppContext) {
    use std::thread;
    use std::time::{Duration, Instant};

    use nmt_terminal::session::TerminalSessionConfig;

    use crate::terminal_tab::settings::TerminalSettings;
    use crate::terminal_tab::view::TerminalLaunch;

    cx.update(|cx| {
        cx.set_global(TerminalSettings {
            improve_powershell_compatibility: true,
            input_style: Default::default(),
            cursor_shape: Default::default(),
            manage_subprocess_job: false,
            command_blocks: false,
            smooth_wheel: false,
            scroll_to_bottom_when_typing: true,
            newline_shortcut: Default::default(),
            font_family: "Consolas".into(),
            font_size: 14.0,
            line_height: 1.2,
            background_opacity: 1.0,
            corner_radius: px(0.0),
            font_fallbacks: Default::default(),
        });
    });

    for (saved, expected) in [
        (Some((132, 43)), (132, 43)),
        (None, (100, 30)),
        (Some((0, 30)), (100, 30)),
        (Some((u16::MAX, 30)), (100, 30)),
    ] {
        let pane = cx.update(|cx| {
            TerminalPane::spawn(cx, 1, TerminalLaunch {
                config: TerminalSessionConfig {
                    shell: Some("pwsh.exe".into()),
                    args: vec![
                        "-NoLogo".into(), "-NoProfile".into(), "-NoExit".into(),
                        "-Command".into(),
                        "$Host.UI.RawUI.WindowTitle = ('NMT_GRID_{0}_{1}' -f [Console]::WindowWidth, [Console]::WindowHeight)".into(),
                    ],
                    ..TerminalSessionConfig::default()
                },
                restorable: TabState { grid_size: saved, ..TabState::default() },
                profile_name: "Startup grid test".into(),
                agent_route: AgentRoute::parse("startup-grid-test").unwrap(),
            }).unwrap()
        });

        let expected_title = format!("NMT_GRID_{}_{}", expected.0, expected.1);
        let deadline = Instant::now() + Duration::from_secs(10);

        loop {
            let title = cx.update(|cx| pane.read(cx).terminal_title());

            if title == expected_title {
                break;
            }

            assert!(
                Instant::now() < deadline,
                "shell reported {title:?}, expected {expected_title}"
            );

            thread::sleep(Duration::from_millis(5));
        }

        cx.update(|cx| {
            pane.update(cx, |pane, _| {
                assert_eq!(pane.tab_state().grid_size, Some(expected));
                assert_eq!(
                    pane.model
                        .source
                        .session
                        .with_render_buffer(|buffer| (buffer.cols(), buffer.rows())),
                    (expected.0 as usize, expected.1 as usize)
                );
                assert!(!pane.model.source.resize_for_content(
                    expected.0 as f32 * 8.0,
                    expected.1 as f32 * 18.0,
                    CellMetrics {
                        width_px: 8.0,
                        height_px: 18.0
                    },
                ));
            });
        });
    }
}

#[gpui::test]
fn keyboard_events_and_native_text_use_the_requested_reporting_mode(cx: &mut TestAppContext) {
    let cx = cx.add_empty_window();
    let pane = pane(cx);
    let (model, input) = controller(b"\x1b[>27u", false);

    cx.update(|window, cx| {
        pane.update(cx, |pane, cx| {
            pane.model = model;

            let mut event = KeyDownEvent {
                keystroke: Keystroke {
                    modifiers: Modifiers::none(),
                    key: "a".into(),
                    key_char: Some("a".into()),
                },
                is_held: false,
                prefer_character_input: false,
            };

            pane.on_key_down(&event, window, cx);

            event.is_held = true;

            pane.on_key_down(&event, window, cx);

            pane.on_key_up(
                &KeyUpEvent {
                    keystroke: event.keystroke.clone(),
                },
                window,
                cx,
            );

            event.prefer_character_input = true;

            pane.on_key_down(&event, window, cx);
            pane.replace_text_in_range(None, "å", window, cx);
        });
    });

    assert_input(
        &input,
        b"\x1b[97;1;97u\x1b[97;1:2;97u\x1b[97;1:3u\x1b[0;1;229u",
    );
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
