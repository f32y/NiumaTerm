use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext, WindowHandle};

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

use nmt_config::profile::{AgentProfile, AgentProfileKind};

use crate::agent_tab::questions::{QuestionEditorState, QuestionPresentation};

use crate::agent_tab::settings::AgentSettings;

use crate::agent_tab::tests::deliver_session_event;

use crate::agent_tab::{AgentPane, AgentThreadDefaults};

fn open_pane(cx: &mut TestAppContext) -> (Entity<AgentPane>, WindowHandle<Root>) {
    let profile = AgentProfile {
        name: "Question Editor Test".into(),
        kind: AgentProfileKind::Codex,
        executable: "missing-question-test-agent.exe".into(),
        ..AgentProfile::default()
    };

    let mut pane = None;

    let window = cx.update(|cx| {
        gpui_component::init(cx);
        cx.set_global(AgentSettings::default());
        cx.set_global(AgentThreadDefaults::default());

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
            let epoch = pane.session.borrow_mut().runtime.begin_start();

            pane.session.borrow_mut().input.starting(epoch);

            let backend = TestBackend::new([], SlashCommandOutcome::NotReady, Vec::new())
                .with_recovery(AgentKind::Codex, "question-thread");

            assert!(matches!(
                pane.session
                    .borrow_mut()
                    .runtime
                    .install(epoch, Ok(Backend::Test(backend))),
                StartOutcome::Installed
            ));

            pane.session.borrow_mut().runtime.ready();
            pane.restore_question_drafts();
        })
    });

    (pane, window)
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
                panic!("ordinary answers use a textarea");
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
        let draft = pane.prompts.questions(&state.input).unwrap();

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

    // Down crosses the question boundary rather than stopping at it, so one
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

    // The first press reaches the first drawn option rather than stepping
    // over it, which is what an out-of-range starting highlight would do.
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
                .questions_mut(&mut pane.session.borrow_mut().input)
                .unwrap()
                .set_text(0, "sensitive".into());

            pane.prepare_question_editors(window, cx);

            assert!(
                pane.prompts.presentations[&pane.session.borrow().input.batches()[0].key()].editors
                    [0]
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
                Some(pane.session.borrow().input.batches()[0].key())
            );

            pane.submit_current_questions(cx);

            assert_eq!(
                pane.session.borrow().input.batches()[0].status(),
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
                pane.prompts.presentations[&pane.session.borrow().input.batches()[0].key()].editors
                    [0]
                .is_none()
            );
            assert_eq!(pane.session.borrow().input.batches()[0].text(0), "");
            assert_eq!(
                pane.prompts.active,
                Some(pane.session.borrow().input.batches()[1].key())
            );
            assert_eq!(pane.session.borrow().input.pending_count(), 1);
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
                .questions_mut(&mut pane.session.borrow_mut().input)
                .unwrap()
                .set_text(0, "keep this".into());

            pane.prepare_question_editors(window, cx);

            let QuestionEditorState::Text(editor) = &pane.prompts.presentations
                [&pane.session.borrow().input.batches()[0].key()]
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
                [&pane.session.borrow().input.batches()[0].key()]
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
                Some(pane.session.borrow().input.batches()[1].key())
            );
            assert_eq!(
                pane.session.borrow().input.batches()[0].text(0),
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
                Some(pane.session.borrow().input.batches()[0].key())
            );
            assert_eq!(
                pane.prompts
                    .questions(&pane.session.borrow().input)
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
                    .questions(&pane.session.borrow().input)
                    .unwrap()
                    .key(),
                old_key
            );

            let QuestionEditorState::Text(editor) = &pane.prompts.presentations
                [&pane.session.borrow().input.batches()[1].key()]
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
                    .questions(&pane.session.borrow().input)
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
                .input
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
            pane.prepare_question_editors(window, cx);

            let old_key = pane.prompts.active.unwrap();

            assert_eq!(pane.prompts.presentations.len(), 1);
            assert!(pane.prompts.presentations[&old_key].editors[0].is_some());

            pane.session.borrow_mut().input.clear_questions();

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
            pane.prepare_question_editors(window, cx);

            let key = pane.prompts.active.unwrap();

            assert_ne!(old_key, key);
            assert!(pane.session.borrow().input.draft(old_key).is_none());
            assert!(!pane.prompts.presentations.contains_key(&old_key));
            assert!(pane.prompts.presentations[&key].editors[0].is_some());
        });
    });
}
