use std::time::Duration;

use gpui_component::input::Enter;
use nmt_agent_utils::chat::QueuedPrompt;
use nmt_config::system::NewlineShortcut;

use crate::agent::composer::prompt_with_response_annotations;
use crate::agent::view::banners::composer_stats_label;
use crate::agent::view::history::queued_message_label;
use crate::agent::view::{ComposerEnterBehavior, composer_enter_behavior};

#[test]
fn queued_message_label_flattens_a_multi_line_prompt() {
    let prompt = QueuedPrompt::local("first\nline".to_string());

    assert_eq!(queued_message_label(&prompt), "Queued message: first line");
}

#[test]
fn queued_message_label_omits_response_annotation_context() {
    let submitted = prompt_with_response_annotations("Explain this", &["selected text".into()]);
    let prompt = QueuedPrompt::local(submitted);

    assert_eq!(
        queued_message_label(&prompt),
        "Queued message: Explain this"
    );
}

#[test]
fn composer_stats_report_only_what_the_conversation_knows() {
    assert_eq!(composer_stats_label(0, 0, None, None), None);

    // A turn that has run but reported nothing else stands on its own.
    assert_eq!(
        composer_stats_label(3, 0, None, None).as_deref(),
        Some("3 turns")
    );

    assert_eq!(
        composer_stats_label(3, 7, Some(Duration::from_millis(820)), Some(94)).as_deref(),
        Some("3 turns · 7 steps · first 820ms · 94% cached")
    );
    assert_eq!(
        composer_stats_label(1, 1, Some(Duration::from_millis(1_240)), None).as_deref(),
        Some("1 turns · 1 steps · first 1.2s")
    );
}

#[test]
fn composer_newline_shortcut_controls_enter_behavior() {
    let plain = Enter {
        secondary: false,
        shift: false,
    };
    let ctrl = Enter {
        secondary: true,
        shift: false,
    };
    let shift = Enter {
        secondary: false,
        shift: true,
    };

    assert_eq!(
        composer_enter_behavior(NewlineShortcut::CtrlEnter, &plain),
        ComposerEnterBehavior::ActivateOrSubmit
    );

    for (shortcut, ctrl_behavior, shift_behavior) in [
        (
            NewlineShortcut::CtrlEnter,
            ComposerEnterBehavior::InsertNewline,
            ComposerEnterBehavior::Submit,
        ),
        (
            NewlineShortcut::ShiftEnter,
            ComposerEnterBehavior::Submit,
            ComposerEnterBehavior::InsertNewline,
        ),
        (
            NewlineShortcut::Off,
            ComposerEnterBehavior::Submit,
            ComposerEnterBehavior::Submit,
        ),
    ] {
        assert_eq!(composer_enter_behavior(shortcut, &ctrl), ctrl_behavior);
        assert_eq!(composer_enter_behavior(shortcut, &shift), shift_behavior);
    }
}
