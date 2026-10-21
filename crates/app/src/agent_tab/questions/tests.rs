use gpui::{
    AppContext as _, BorrowAppContext as _, Entity, TestAppContext, VisualTestContext, WindowHandle,
};
use gpui_component::Root;
use gpui_component::input::InputEvent;
use nmt_agent::AgentWorkspace;
use nmt_agent::chat::{
    Event, Question, QuestionInput, QuestionMode, QuestionOption, QuestionRequest,
    QuestionResolution, SlashCommandOutcome,
};
use nmt_agent::session::input::{QuestionDraft, QuestionStatus};
use nmt_agent::session::lifecycle::StartOutcome;
use nmt_agent::session::test_support::TestBackend;
use nmt_agent::session::{AgentKind, Backend};
use nmt_config::profile::AgentProfile;
use nmt_config::system::NewlineShortcut;

use crate::agent_tab::AgentPane;
use crate::agent_tab::questions::{QuestionEditorState, QuestionPresentation};
use crate::agent_tab::settings::AgentSettings;
use crate::agent_tab::tests::deliver_session_event;

fn open_pane(cx: &mut TestAppContext) -> (Entity<AgentPane>, WindowHandle<Root>) {
    let profile = AgentProfile {
        name: "Question Editor Test".into(),
        kind: AgentKind::Codex,
        executable: "missing-question-test-agent.exe".into(),
        ..AgentProfile::default()
    };

    let mut pane = None;

    let window = cx.update(|cx| {
        gpui_component::init(cx);

        cx.set_global(AgentSettings::default());

        cx.open_window(Default::default(), |window, cx| {
            let agent = cx.new(|cx| AgentPane::new(profile, AgentWorkspace::default(), window, cx));

            pane = Some(agent.clone());

            cx.new(|cx| Root::new(agent, window, cx))
        })
        .expect("open question editor test window")
    });

    let pane = pane.expect("create agent pane");

    cx.update(|cx| {
        pane.update(cx, |pane, _| {
            let epoch = pane.session.borrow_mut().starting(None);

            let backend = TestBackend::new([], SlashCommandOutcome::NotReady, Vec::new())
                .with_recovery(AgentKind::Codex, "question-thread");

            assert!(matches!(
                pane.session
                    .borrow_mut()
                    .runtime_mut()
                    .install(epoch, Ok(Backend::Test(backend))),
                StartOutcome::Installed
            ));

            pane.session.borrow_mut().runtime_mut().ready();

            pane.restore_question_drafts();
        })
    });

    (pane, window)
}

/// The keystroke the `ctrl-enter` newline shortcut is pressed with: the text
/// input binds its secondary Enter to Command on macOS and Control elsewhere.
#[cfg(target_os = "macos")]
const SECONDARY_ENTER: &str = "cmd-enter";

#[cfg(not(target_os = "macos"))]
const SECONDARY_ENTER: &str = "ctrl-enter";

#[gpui::test]
fn question_editor_enter_uses_current_newline_setting(cx: &mut TestAppContext) {
    for shortcut in [
        NewlineShortcut::CtrlEnter,
        NewlineShortcut::ShiftEnter,
        NewlineShortcut::Off,
    ] {
        for key in ["enter", SECONDARY_ENTER, "shift-enter"] {
            let (pane, window) = open_pane(cx);

            let mut cx = VisualTestContext::from_window(window.into(), cx);

            deliver_session_event(
                &pane,
                Event::InputRequested(QuestionRequest {
                    id: "keyboard-answer".into(),
                    mode: QuestionMode::Blocking,
                    questions: vec![Question {
                        input: QuestionInput::Text,
                        ..question("Describe the change", false, &[])
                    }],
                }),
                &cx,
            );

            let editor = cx.update(|window, cx| {
                pane.update(cx, |pane, cx| {
                    pane.prompts.prepare_editors(&pane.session, window, cx);

                    let active = pane.prompts.active.unwrap();

                    let QuestionEditorState::Text(editor) = &pane.prompts.presentations[&active]
                        .editors[0]
                        .as_ref()
                        .unwrap()
                        .state
                    else {
                        panic!("text editor required");
                    };

                    editor.update(cx, |editor, cx| {
                        editor.set_value("answer", window, cx);

                        cx.emit(InputEvent::Change);
                        editor.focus(window, cx);
                    });

                    editor.clone()
                })
            });

            cx.update(|_, cx| {
                cx.update_global::<AgentSettings, _>(|settings, _| {
                    settings.newline_shortcut = shortcut;
                });
            });

            cx.run_until_parked();
            cx.simulate_keystrokes(key);
            cx.run_until_parked();

            let newline = match shortcut {
                NewlineShortcut::CtrlEnter => key == SECONDARY_ENTER,
                NewlineShortcut::ShiftEnter => key == "shift-enter",
                NewlineShortcut::Off => false,
            };

            cx.update(|_, cx| {
                pane.update(cx, |pane, cx| {
                    let session = pane.session.borrow();
                    let draft = &session.input().batches()[0];

                    assert_eq!(
                        draft.status(),
                        if newline {
                            QuestionStatus::Pending
                        } else {
                            QuestionStatus::Submitting
                        },
                        "{shortcut:?}: {key}"
                    );
                    assert_eq!(editor.read(cx).value().contains('\n'), newline);
                    assert_eq!(draft.text(0), editor.read(cx).value().as_ref());
                });
            });
        }
    }
}

#[gpui::test]
fn question_editors_keep_multiline_text_and_mask_secrets(cx: &mut TestAppContext) {
    let (pane, window) = open_pane(cx);

    let mut cx = VisualTestContext::from_window(window.into(), cx);
    let mut plain = question("Describe the change", false, &[]);

    plain.input = QuestionInput::Text;

    let mut secret = question("Enter a token", false, &[]);

    secret.input = QuestionInput::Secret;

    deliver_session_event(
        &pane,
        Event::InputRequested(QuestionRequest {
            id: "editors".into(),
            mode: QuestionMode::Blocking,
            questions: vec![plain, secret],
        }),
        &cx,
    );

    cx.update(|window, cx| {
        pane.update(cx, |pane, cx| {
            let prompt =
                &pane.prompts.presentations[&pane.prompts.active.expect("active questions")];

            let QuestionEditorState::Text(plain) =
                &prompt.editors[0].as_ref().expect("plain editor").state
            else {
                panic!("plain answers use a textarea");
            };

            plain.update(cx, |plain, cx| {
                plain.set_value("first line\nsecond line", window, cx);

                cx.emit(InputEvent::Change);
            });

            let plain = plain.read(cx);

            assert!(plain.is_multi_line());
            assert!(!plain.presentation().is_masked());
            assert_eq!(plain.value().as_ref(), "first line\nsecond line");

            let QuestionEditorState::Secret(secret) =
                &prompt.editors[1].as_ref().expect("secret editor").state
            else {
                panic!("secret answers use a password input");
            };

            secret.update(cx, |secret, cx| {
                secret.set_value("test-token", window, cx);

                cx.emit(InputEvent::Change);

                secret.select_all(window, cx);
            });

            let secret = secret.read(cx);

            assert!(secret.is_single_line());
            assert!(secret.presentation().is_masked());
            assert!(secret.context_menu_capabilities().has_selection());
            assert!(!secret.context_menu_capabilities().is_copyable());
            assert_eq!(secret.value().as_ref(), "test-token");
        });
    });

    cx.read(|cx| {
        let pane = pane.read(cx);
        let state = pane.session.borrow();
        let draft = pane.prompts.questions(state.input()).unwrap();

        assert_eq!(draft.text(0), "first line\nsecond line");
        assert_eq!(draft.text(1), "test-token");
    });
}

fn question(text: &str, multi_select: bool, labels: &[&str]) -> Question {
    Question {
        input: Default::default(),
        header: None,
        question: text.to_owned(),
        multi_select,
        options: labels
            .iter()
            .map(|label| QuestionOption {
                label: (*label).to_owned(),
                description: None,
            })
            .collect(),
    }
}

#[test]
fn the_highlight_walks_every_option_across_questions_and_wraps() {
    let mut prompt = QuestionDraft::new(
        "draft".into(),
        vec![
            question("Which database?", false, &["Postgres", "SQLite"]),
            question("Which extras?", true, &["Metrics", "Tracing"]),
        ],
    );

    let mut presentation = QuestionPresentation::new(&prompt);

    assert!(presentation.is_focused(0, 0));

    // Down crosses the question boundary instead of stopping at it, so one
    // pair of keys reaches every option on the card.
    let walked: Vec<(usize, usize)> = (0..4)
        .map(|_| {
            presentation.move_focus(&mut prompt, true);

            presentation.focus
        })
        .collect();

    assert_eq!(walked, vec![(0, 1), (1, 0), (1, 1), (0, 0)]);

    // Up from the first option wraps to the last.
    presentation.move_focus(&mut prompt, false);

    assert_eq!(presentation.focus, (1, 1));
}

#[test]
fn a_question_with_no_options_cannot_trap_the_highlight() {
    // The provider caps options at four but does not promise a minimum, and
    // a card that swallows the arrow keys would leave the user no way to
    // reach the options that do exist.
    let mut prompt = QuestionDraft::new(
        "draft".into(),
        vec![
            question("Nothing to pick", false, &[]),
            question("Which database?", false, &["Postgres", "SQLite"]),
        ],
    );

    // The first press reaches the first drawn option. An out-of-range
    // starting highlight would step over it instead.
    let mut presentation = QuestionPresentation::new(&prompt);

    assert!(presentation.move_focus(&mut prompt, true));
    assert_eq!(presentation.focus, (1, 0));

    // A card with nothing to pick consumes no keys, so they still reach
    // whatever else is listening.
    let empty =
        &mut QuestionDraft::new("draft".into(), vec![question("Nothing at all", false, &[])]);

    let mut presentation = QuestionPresentation::new(empty);

    assert!(!presentation.move_focus(empty, true));
}

#[gpui::test]
fn confirmed_secret_answer_releases_its_widget_and_reveals_the_next_batch(cx: &mut TestAppContext) {
    let (pane, window) = open_pane(cx);

    let mut cx = VisualTestContext::from_window(window.into(), cx);
    let mut secret = question("Token", false, &[]);

    secret.input = QuestionInput::Secret;

    deliver_session_event(
        &pane,
        Event::InputRequested(QuestionRequest {
            id: "secret".into(),
            mode: QuestionMode::Blocking,
            questions: vec![secret],
        }),
        &cx,
    );

    cx.update(|window, cx| {
        pane.update(cx, |pane, cx| {
            pane.prompts
                .questions_mut(pane.session.borrow_mut().input_mut())
                .unwrap()
                .set_text(0, "sensitive".into());

            pane.prompts.prepare_editors(&pane.session, window, cx);

            assert!(
                pane.prompts.presentations[&pane.session.borrow().input().batches()[0].key()]
                    .editors[0]
                    .is_some()
            );
        });
    });

    deliver_session_event(
        &pane,
        Event::InputRequested(QuestionRequest {
            id: "next".into(),
            mode: QuestionMode::Async,
            questions: vec![question("Next", false, &["yes"])],
        }),
        &cx,
    );

    cx.update(|_, cx| {
        pane.update(cx, |pane, cx| {
            assert_eq!(
                pane.prompts.active,
                Some(pane.session.borrow().input().batches()[0].key())
            );

            pane.submit_current_questions(cx);

            assert_eq!(
                pane.session.borrow().input().batches()[0].status(),
                QuestionStatus::Submitting
            );
        });
    });

    deliver_session_event(
        &pane,
        Event::InputResolved {
            id: "secret".into(),
            resolution: QuestionResolution::Submitted {
                message: None,
                started_turn: false,
            },
        },
        &cx,
    );

    cx.update(|_, cx| {
        pane.update(cx, |pane, _| {
            assert!(
                pane.prompts.presentations[&pane.session.borrow().input().batches()[0].key()]
                    .editors[0]
                    .is_none()
            );
            assert_eq!(pane.session.borrow().input().batches()[0].text(0), "");
            assert_eq!(
                pane.prompts.active,
                Some(pane.session.borrow().input().batches()[1].key())
            );
            assert_eq!(pane.session.borrow().input().pending_count(), 1);
        });
    });
}

#[gpui::test]
fn blocking_requests_reveal_without_discarding_async_drafts_and_duplicates_keep_widgets(
    cx: &mut TestAppContext,
) {
    let (pane, window) = open_pane(cx);

    let mut cx = VisualTestContext::from_window(window.into(), cx);

    let asynchronous = QuestionRequest {
        id: "async".into(),
        mode: QuestionMode::Async,
        questions: vec![Question {
            input: QuestionInput::Text,
            ..question("Async", false, &[])
        }],
    };

    deliver_session_event(&pane, Event::InputRequested(asynchronous.clone()), &cx);

    let editor = cx.update(|window, cx| {
        pane.update(cx, |pane, cx| {
            pane.prompts
                .questions_mut(pane.session.borrow_mut().input_mut())
                .unwrap()
                .set_text(0, "keep this".into());

            pane.prompts.prepare_editors(&pane.session, window, cx);

            let QuestionEditorState::Text(editor) = &pane.prompts.presentations
                [&pane.session.borrow().input().batches()[0].key()]
                .editors[0]
                .as_ref()
                .unwrap()
                .state
            else {
                panic!("text editor required");
            };

            editor.clone()
        })
    });

    deliver_session_event(&pane, Event::InputRequested(asynchronous), &cx);

    cx.update(|_, cx| {
        pane.update(cx, |pane, _| {
            let QuestionEditorState::Text(current) = &pane.prompts.presentations
                [&pane.session.borrow().input().batches()[0].key()]
                .editors[0]
                .as_ref()
                .unwrap()
                .state
            else {
                panic!("text editor required");
            };

            assert_eq!(editor, *current);
        });
    });

    deliver_session_event(
        &pane,
        Event::InputRequested(QuestionRequest {
            id: "blocking".into(),
            mode: QuestionMode::Blocking,
            questions: vec![question("Blocking", false, &["yes"])],
        }),
        &cx,
    );

    cx.update(|_, cx| {
        pane.update(cx, |pane, cx| {
            assert_eq!(
                pane.prompts.active,
                Some(pane.session.borrow().input().batches()[1].key())
            );
            assert_eq!(
                pane.session.borrow().input().batches()[0].text(0),
                "keep this"
            );

            pane.skip_current_questions(cx);
        });
    });

    deliver_session_event(
        &pane,
        Event::InputResolved {
            id: "blocking".into(),
            resolution: QuestionResolution::Skipped,
        },
        &cx,
    );

    cx.update(|_, cx| {
        pane.update(cx, |pane, _| {
            assert_eq!(
                pane.prompts.active,
                Some(pane.session.borrow().input().batches()[0].key())
            );
            assert_eq!(
                pane.prompts
                    .questions(pane.session.borrow().input())
                    .unwrap()
                    .text(0),
                "keep this"
            );
        });
    });
}

#[gpui::test]
fn a_new_request_does_not_reuse_the_expired_answer(cx: &mut TestAppContext) {
    let (pane, window) = open_pane(cx);

    let mut cx = VisualTestContext::from_window(window.into(), cx);
    let mut question = question("Answer", false, &[]);

    question.input = QuestionInput::Text;

    deliver_session_event(
        &pane,
        Event::InputRequested(QuestionRequest {
            id: "old".into(),
            mode: QuestionMode::Blocking,
            questions: vec![question.clone()],
        }),
        &cx,
    );

    let (old_key, old_editor) = cx.update(|window, cx| {
        pane.update(cx, |pane, cx| {
            let key = pane.prompts.active.unwrap();

            let QuestionEditorState::Text(editor) = &pane.prompts.presentations[&key].editors[0]
                .as_ref()
                .unwrap()
                .state
            else {
                panic!("text editor");
            };

            editor.update(cx, |editor, cx| editor.set_value("old value", window, cx));

            (key, editor.clone())
        })
    });

    deliver_session_event(
        &pane,
        Event::InputResolved {
            id: "old".into(),
            resolution: QuestionResolution::Expired,
        },
        &cx,
    );

    deliver_session_event(
        &pane,
        Event::InputRequested(QuestionRequest {
            id: "new".into(),
            mode: QuestionMode::Blocking,
            questions: vec![question],
        }),
        &cx,
    );

    cx.update(|_, cx| {
        pane.update(cx, |pane, cx| {
            assert_ne!(
                pane.prompts
                    .questions(pane.session.borrow().input())
                    .unwrap()
                    .key(),
                old_key
            );

            let QuestionEditorState::Text(editor) = &pane.prompts.presentations
                [&pane.session.borrow().input().batches()[1].key()]
                .editors[0]
                .as_ref()
                .unwrap()
                .state
            else {
                panic!("text editor required");
            };

            assert_ne!(*editor, old_editor);
            assert_eq!(editor.read(cx).value().as_ref(), "");
            assert_eq!(
                pane.prompts
                    .questions(pane.session.borrow().input())
                    .unwrap()
                    .text(0),
                ""
            );
        });
    });
}

#[gpui::test]
fn question_editors_survive_unshown_batches_and_reused_positions(cx: &mut TestAppContext) {
    let (pane, window) = open_pane(cx);

    let mut cx = VisualTestContext::from_window(window.into(), cx);
    let mut prompt = question("Answer", false, &[]);

    prompt.input = QuestionInput::Text;

    cx.update(|_, cx| {
        pane.update(cx, |pane, _| {
            pane.session
                .borrow_mut()
                .input_mut()
                .history("unshown", vec![prompt.clone()]);
        });
    });

    deliver_session_event(
        &pane,
        Event::InputRequested(QuestionRequest {
            id: "shown".into(),
            mode: QuestionMode::Blocking,
            questions: vec![prompt.clone()],
        }),
        &cx,
    );

    let old_key = cx.update(|window, cx| {
        pane.update(cx, |pane, cx| {
            pane.prompts.prepare_editors(&pane.session, window, cx);

            let old_key = pane.prompts.active.unwrap();

            assert_eq!(pane.prompts.presentations.len(), 1);
            assert!(pane.prompts.presentations[&old_key].editors[0].is_some());

            pane.session.borrow_mut().clear_conversation();

            old_key
        })
    });

    deliver_session_event(
        &pane,
        Event::InputRequested(QuestionRequest {
            id: "replacement".into(),
            mode: QuestionMode::Blocking,
            questions: vec![prompt],
        }),
        &cx,
    );

    cx.update(|window, cx| {
        pane.update(cx, |pane, cx| {
            pane.prompts.prepare_editors(&pane.session, window, cx);

            let key = pane.prompts.active.unwrap();

            assert_ne!(old_key, key);
            assert!(pane.session.borrow().input().draft(old_key).is_none());
            assert!(!pane.prompts.presentations.contains_key(&old_key));
            assert!(pane.prompts.presentations[&key].editors[0].is_some());
        });
    });
}

#[test]
fn the_highlight_stays_on_the_question_shown_alone() {
    let mut prompt = QuestionDraft::new(
        "draft".into(),
        vec![
            question("Which database?", false, &["Postgres", "SQLite"]),
            question("Which extras?", true, &["Metrics", "Tracing"]),
        ],
    );

    let mut presentation = QuestionPresentation::new(&prompt);

    presentation.page = Some(1);

    let walked: Vec<(usize, usize)> = (0..3)
        .map(|_| {
            presentation.move_focus(&mut prompt, true);

            presentation.focus
        })
        .collect();

    assert_eq!(walked, vec![(1, 0), (1, 1), (1, 0)]);
}

#[gpui::test]
fn a_batch_answered_one_question_at_a_time_moves_on_only_from_an_answer(cx: &mut TestAppContext) {
    let (pane, window) = open_pane(cx);

    let mut cx = VisualTestContext::from_window(window.into(), cx);

    cx.update(|_, cx| {
        cx.update_global::<AgentSettings, _>(|settings, _| {
            settings.answer_questions_one_at_a_time = true;
        });
    });

    deliver_session_event(
        &pane,
        Event::InputRequested(QuestionRequest {
            id: "pair".into(),
            mode: QuestionMode::Blocking,
            questions: vec![
                question("Which database?", false, &["Postgres", "SQLite"]),
                question("Which extras?", true, &["Metrics", "Tracing"]),
            ],
        }),
        &cx,
    );

    cx.update(|window, cx| {
        pane.update(cx, |pane, cx| {
            let key = pane.session.borrow().input().batches()[0].key();
            let session = pane.session.clone();

            pane.prompts.render(&session, true, window, cx);

            assert_eq!(pane.prompts.presentations[&key].page, Some(0));

            // The shown question has no answer yet, so there is nowhere to go.
            assert!(!pane.prompts.step(pane.session.borrow().input(), true));

            pane.toggle_question_option(0, 1, cx);

            assert!(pane.prompts.step(pane.session.borrow().input(), true));
            assert_eq!(pane.prompts.presentations[&key].page, Some(1));
            assert_eq!(pane.prompts.presentations[&key].focus, (1, 0));

            // The last question has nowhere further to go, so Enter submits
            // once it is answered.
            pane.toggle_question_option(1, 0, cx);
            pane.advance_or_submit_questions(cx);

            assert_eq!(
                pane.session.borrow().input().batches()[0].status(),
                QuestionStatus::Submitting
            );
        });
    });
}

#[gpui::test]
fn an_answered_batch_yields_the_card_to_a_question_that_still_needs_one(cx: &mut TestAppContext) {
    fn ask(cx: &VisualTestContext, pane: &Entity<AgentPane>, id: &str, label: &str) {
        deliver_session_event(
            pane,
            Event::InputRequested(QuestionRequest {
                id: id.into(),
                mode: QuestionMode::Blocking,
                questions: vec![question("Choose", false, &[label])],
            }),
            cx,
        );
    }

    let (pane, window) = open_pane(cx);

    let mut cx = VisualTestContext::from_window(window.into(), cx);

    ask(&cx, &pane, "first", "one");
    ask(&cx, &pane, "second", "two");

    cx.update(|window, cx| {
        pane.update(cx, |pane, cx| {
            let session = pane.session.clone();

            // The second request arrives while the first one is unanswered, so
            // the card stays on the batch the turn is blocked on.
            pane.prompts.render(&session, true, window, cx);

            assert_eq!(
                pane.prompts.active,
                Some(session.borrow().input().batches()[0].key())
            );

            pane.toggle_question_option(0, 0, cx);
            pane.submit_current_questions(cx);

            assert_eq!(
                session.borrow().input().batches()[0].status(),
                QuestionStatus::Submitting
            );

            // Every control of an answered batch is disabled until the harness
            // confirms the answer, so the card has to move to the question that
            // still needs one.
            pane.prompts.render(&session, true, window, cx);

            assert_eq!(
                pane.prompts.active,
                Some(session.borrow().input().batches()[1].key())
            );
        });
    });

    // The harness confirms the first answer, and that confirmation is the only
    // thing that lets the next question be answered at all. A backend confirms
    // one answer at a time, so an unconfirmed one refuses every other batch.
    deliver_session_event(
        &pane,
        Event::InputResolved {
            id: "first".into(),
            resolution: QuestionResolution::Submitted {
                message: None,
                started_turn: false,
            },
        },
        &cx,
    );

    cx.update(|_, cx| {
        pane.update(cx, |pane, cx| {
            let session = pane.session.clone();

            pane.toggle_question_option(0, 0, cx);
            pane.submit_current_questions(cx);

            assert_eq!(
                session.borrow().input().batches()[1].status(),
                QuestionStatus::Submitting
            );
        });
    });

    // A question that arrives while an answer is unconfirmed takes the card on
    // arrival, without waiting for a frame to find the waiting question.
    ask(&cx, &pane, "third", "three");

    cx.update(|_, cx| {
        pane.update(cx, |pane, _| {
            let session = pane.session.clone();

            assert_eq!(
                pane.prompts.active,
                Some(session.borrow().input().batches()[2].key())
            );
        });
    });
}

#[gpui::test]
fn a_question_that_accepts_text_offers_its_input_beside_the_options(cx: &mut TestAppContext) {
    let (pane, window) = open_pane(cx);

    let mut cx = VisualTestContext::from_window(window.into(), cx);

    deliver_session_event(
        &pane,
        Event::InputRequested(QuestionRequest {
            id: "mixed".into(),
            mode: QuestionMode::Blocking,
            questions: vec![
                Question {
                    input: QuestionInput::SelectionOnly,
                    ..question("Which database?", false, &["Postgres", "SQLite"])
                },
                Question {
                    input: QuestionInput::Text,
                    ..question("Anything the options miss?", false, &["No"])
                },
            ],
        }),
        &cx,
    );

    cx.update(|window, cx| {
        pane.update(cx, |pane, cx| {
            pane.prompts.prepare_editors(&pane.session, window, cx);

            let active = pane.prompts.active.expect("the batch is shown");
            let editors = &pane.prompts.presentations[&active].editors;

            // An option-only question has nowhere for typed text to go, so it
            // keeps its options alone instead of collecting an answer nothing
            // reads. A question the harness answers with text as well offers the
            // input as part of the card, without the user having to select
            // "Custom answer" first.
            assert!(editors[0].is_none());
            assert!(editors[1].is_some());
        });
    });
}

#[gpui::test]
fn typing_in_the_card_answers_the_question_with_that_text(cx: &mut TestAppContext) {
    let (pane, window) = open_pane(cx);

    let mut cx = VisualTestContext::from_window(window.into(), cx);

    deliver_session_event(
        &pane,
        Event::InputRequested(QuestionRequest {
            id: "typed".into(),
            mode: QuestionMode::Blocking,
            questions: vec![Question {
                input: QuestionInput::Text,
                ..question("Which database?", false, &["Postgres", "SQLite"])
            }],
        }),
        &cx,
    );

    cx.update(|window, cx| {
        pane.update(cx, |pane, cx| {
            pane.prompts.prepare_editors(&pane.session, window, cx);

            let active = pane.prompts.active.expect("the batch is shown");

            let editor = pane.prompts.presentations[&active].editors[0]
                .as_ref()
                .expect("a question that accepts text has an input");

            let QuestionEditorState::Text(state) = &editor.state else {
                panic!("text editor required");
            };

            // Typing needs the box focused, and a click on it is how that
            // happens.
            state.update(cx, |state, cx| state.focus(window, cx));
        });
    });

    // A keystroke is what an input reports as a change; `set_value` sets the
    // text without reporting one, so it cannot stand in for typing.
    cx.simulate_input("neither: SQLite in WAL mode");

    cx.update(|_, cx| {
        pane.update(cx, |pane, cx| {
            let active = pane.prompts.active.expect("the batch is shown");

            // Typing is the answer, not a note beside the options: the card
            // reports it as the custom answer, and the question counts as
            // answered without any option being picked.
            let typed = pane
                .session
                .borrow()
                .input()
                .draft(active)
                .map(|draft| (draft.text(0).to_string(), draft.is_custom(0)));

            assert_eq!(
                typed,
                Some(("neither: SQLite in WAL mode".to_string(), true))
            );

            pane.submit_current_questions(cx);

            assert_eq!(
                pane.session.borrow().input().batches()[0].status(),
                QuestionStatus::Submitting,
                "the typed answer went to the backend"
            );
        });
    });
}

#[gpui::test]
fn the_answer_box_stands_beside_a_choice_circle_of_its_own(cx: &mut TestAppContext) {
    let (pane, window) = open_pane(cx);

    let mut cx = VisualTestContext::from_window(window.into(), cx);

    deliver_session_event(
        &pane,
        Event::InputRequested(QuestionRequest {
            id: "choice".into(),
            mode: QuestionMode::Blocking,
            questions: vec![Question {
                input: QuestionInput::Text,
                ..question("Which database?", false, &["Postgres", "SQLite"])
            }],
        }),
        &cx,
    );

    cx.run_until_parked();

    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    let circle = cx
        .debug_bounds("question-text-choice-0")
        .expect("the answer box keeps the choice circle the option rows use");

    cx.simulate_click(circle.center(), Default::default());

    cx.run_until_parked();

    cx.update(|_, cx| {
        pane.update(cx, |pane, _| {
            let active = pane.prompts.active.expect("the batch is shown");

            // Picking the circle says the typed text is the answer, exactly
            // as an option's circle says it for the label beside it.
            assert!(
                pane.session
                    .borrow()
                    .input()
                    .draft(active)
                    .is_some_and(|draft| draft.is_custom(0)),
                "picking the answer-box circle answers the question with text"
            );
        });
    });
}
