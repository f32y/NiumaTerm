use gpui::{Keystroke, Modifiers};
use nmt_config::system::NewlineShortcut;

use crate::input::{key_action, pty_bytes_for_key, should_defer_to_ime};
use crate::surface::TerminalKeyAction;

fn key(name: &str, key_char: Option<&str>) -> Keystroke {
    Keystroke {
        modifiers: Modifiers::none(),
        key: name.to_string(),
        key_char: key_char.map(str::to_string),
    }
}

fn modified(name: &str, key_char: Option<&str>, modifiers: Modifiers) -> Keystroke {
    let mut key = key(name, key_char);
    key.modifiers = modifiers;
    key
}

fn bytes(name: &str, key_char: Option<&str>) -> Vec<u8> {
    match key_action(&key(name, key_char), NewlineShortcut::CtrlEnter) {
        TerminalKeyAction::Write(bytes) => bytes,
        action => panic!("expected write action, got {action:?}"),
    }
}

#[test]
fn printable_key_sends_key_char() {
    assert_eq!(bytes("a", Some("a")), b"a");
}

#[test]
fn basic_control_keys_send_terminal_bytes() {
    assert_eq!(bytes("enter", None), b"\r");
    assert_eq!(bytes("tab", None), b"\t");
    assert_eq!(bytes("backspace", None), b"\x7f");
    assert_eq!(bytes("escape", None), b"\x1b");
}

#[test]
fn legacy_enter_modifiers_match_windows_terminal() {
    let mut ctrl_alt = Modifiers::control();
    ctrl_alt.alt = true;

    assert_eq!(
        pty_bytes_for_key(
            &modified("enter", Some("\r"), Modifiers::none()),
            NewlineShortcut::CtrlEnter,
        )
        .as_deref(),
        Some(&b"\r"[..])
    );
    assert_eq!(
        pty_bytes_for_key(
            &modified("enter", Some("\r"), Modifiers::control()),
            NewlineShortcut::CtrlEnter,
        )
        .as_deref(),
        Some(&b"\n"[..])
    );
    assert_eq!(
        pty_bytes_for_key(
            &modified("enter", Some("\r"), Modifiers::alt()),
            NewlineShortcut::CtrlEnter,
        )
        .as_deref(),
        Some(&b"\x1b\r"[..])
    );
    assert_eq!(
        pty_bytes_for_key(
            &modified("enter", Some("\r"), ctrl_alt),
            NewlineShortcut::CtrlEnter,
        )
        .as_deref(),
        Some(&b"\x1b\n"[..])
    );
}

#[test]
fn newline_shortcut_controls_modified_enter() {
    let ctrl_enter = modified("enter", Some("\r"), Modifiers::control());
    let shift_enter = modified("enter", Some("\r"), Modifiers::shift());

    for (shortcut, ctrl_bytes, shift_bytes) in [
        (NewlineShortcut::CtrlEnter, b'\n', b'\r'),
        (NewlineShortcut::ShiftEnter, b'\r', b'\n'),
        (NewlineShortcut::Off, b'\r', b'\r'),
    ] {
        assert_eq!(
            pty_bytes_for_key(&ctrl_enter, shortcut).as_deref(),
            Some(&[ctrl_bytes][..])
        );
        assert_eq!(
            pty_bytes_for_key(&shift_enter, shortcut).as_deref(),
            Some(&[shift_bytes][..])
        );
    }
}

#[test]
fn navigation_keys_use_terminal_sequences() {
    assert_eq!(bytes("left", None), b"\x1b[D");
    assert_eq!(bytes("right", None), b"\x1b[C");
    assert_eq!(bytes("up", None), b"\x1b[A");
    assert_eq!(bytes("down", None), b"\x1b[B");
    assert_eq!(bytes("home", None), b"\x1b[H");
    assert_eq!(bytes("end", None), b"\x1b[F");
    assert_eq!(bytes("pageup", None), b"\x1b[5~");
    assert_eq!(bytes("pagedown", None), b"\x1b[6~");
}

#[test]
fn function_keys_use_terminal_sequences() {
    assert_eq!(bytes("f1", None), b"\x1bOP");
    assert_eq!(bytes("f4", None), b"\x1bOS");
    assert_eq!(bytes("f5", None), b"\x1b[15~");
    assert_eq!(bytes("f12", None), b"\x1b[24~");
}

#[test]
fn modified_named_keys_include_modifier_parameters() {
    let mut ctrl_left = key("left", None);
    ctrl_left.modifiers.control = true;

    assert_eq!(
        pty_bytes_for_key(&ctrl_left, NewlineShortcut::CtrlEnter).as_deref(),
        Some(&b"\x1b[1;5D"[..])
    );

    let mut shift_tab = key("tab", None);
    shift_tab.modifiers.shift = true;
    assert_eq!(
        pty_bytes_for_key(&shift_tab, NewlineShortcut::CtrlEnter).as_deref(),
        Some(&b"\x1b[Z"[..])
    );
}

#[test]
fn plain_ctrl_chords_send_legacy_control_bytes() {
    // The platform strips control characters from `key_char`, so these
    // ride the `legacy_ctrl_byte` fallback.
    for (name, byte) in [("z", 0x1au8), ("d", 0x04), ("\\", 0x1c)] {
        assert_eq!(
            pty_bytes_for_key(
                &modified(name, None, Modifiers::control()),
                NewlineShortcut::CtrlEnter,
            )
            .as_deref(),
            Some(&[byte][..]),
            "ctrl-{name}"
        );
    }
    // Ctrl-Shift chords stay with the UI (copy/paste shortcuts).
    let mut ctrl_shift = Modifiers::control();
    ctrl_shift.shift = true;
    assert_eq!(
        pty_bytes_for_key(&modified("x", None, ctrl_shift), NewlineShortcut::CtrlEnter,),
        None
    );
}

#[test]
fn plain_text_defers_to_ime_but_special_keys_do_not() {
    // Plain printable char: char path handles it, so on_key_down must defer.
    assert!(should_defer_to_ime(&key("a", Some("a"))));
    assert!(should_defer_to_ime(&modified(
        "A",
        Some("A"),
        Modifiers::shift()
    )));
    // Named keys and modified keys still encode in on_key_down.
    assert!(!should_defer_to_ime(&key("enter", Some("\r"))));
    assert!(!should_defer_to_ime(&key("tab", Some("\t"))));
    assert!(!should_defer_to_ime(&modified(
        "c",
        Some("c"),
        Modifiers::control()
    )));
    // A key with no committed character never defers.
    assert!(!should_defer_to_ime(&key("left", None)));
}

#[test]
fn copy_paste_shortcuts_are_actions_not_pty_bytes() {
    let copy = modified("c", Some("c"), Modifiers::control());
    let paste = modified("v", Some("v"), Modifiers::control());

    assert_eq!(
        key_action(&copy, NewlineShortcut::CtrlEnter),
        TerminalKeyAction::CopyOrWrite(vec![0x03])
    );
    assert_eq!(
        key_action(&paste, NewlineShortcut::CtrlEnter),
        TerminalKeyAction::Paste
    );
    assert_eq!(pty_bytes_for_key(&copy, NewlineShortcut::CtrlEnter), None);
}

#[test]
fn ctrl_shift_copy_paste_are_no_longer_app_shortcuts() {
    let copy = modified("c", Some("c"), Modifiers::control_shift());
    let paste = modified("v", Some("v"), Modifiers::control_shift());

    assert_eq!(
        key_action(&copy, NewlineShortcut::CtrlEnter),
        TerminalKeyAction::Ignore
    );
    assert_eq!(
        key_action(&paste, NewlineShortcut::CtrlEnter),
        TerminalKeyAction::Ignore
    );
}
