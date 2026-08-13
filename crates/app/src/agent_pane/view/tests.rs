use std::collections::VecDeque;

use gpui_component::input::Enter;
use nmt_config::system::NewlineShortcut;

use crate::agent_pane::view::{
    ComposerEnterBehavior, composer_enter_behavior, queued_message_label,
};

#[test]
fn queued_message_label_keeps_order_and_flattens_lines() {
    let messages = VecDeque::from(["first\nline".to_string(), "second".to_string()]);

    assert_eq!(
        queued_message_label(&messages).as_deref(),
        Some("Queued message: first line · second")
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
