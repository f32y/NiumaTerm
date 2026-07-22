use gpui::{Keystroke, Modifiers};
use nmt_input::event::ElementState;
use nmt_input::keyboard::{Key, KeyLocation, ModifiersState, NamedKey};
use nmt_input::{KeyEncodeFlags, KeyInput, encode_terminal_input};

use crate::terminal::surface::TerminalKeyAction;

#[cfg(test)]
pub(crate) fn pty_bytes_for_key(event: &Keystroke) -> Option<Vec<u8>> {
    match key_action(event) {
        TerminalKeyAction::Write(bytes) => Some(bytes),
        TerminalKeyAction::CopyOrWrite(_)
        | TerminalKeyAction::Paste
        | TerminalKeyAction::Ignore => None,
    }
}

/// Whether a keystroke is plain printable text that the platform delivers
/// through the char/IME path (`replace_text_in_range`). Such keys must NOT also
/// be encoded in `on_key_down`, or the character is written to the PTY twice.
/// Named keys (Enter, Tab, arrows, …) and modified keys still encode normally.
pub(crate) fn should_defer_to_ime(event: &Keystroke) -> bool {
    event.key_char.is_some()
        && !event.modifiers.control
        && !event.modifiers.alt
        && !event.modifiers.platform
        && named_key(&event.key).is_none()
}

pub(crate) fn key_action(event: &Keystroke) -> TerminalKeyAction {
    if is_clipboard_shortcut(event, "c") {
        return TerminalKeyAction::CopyOrWrite(vec![0x03]);
    }
    if is_clipboard_shortcut(event, "v") {
        return TerminalKeyAction::Paste;
    }
    if is_old_clipboard_shortcut(event) {
        return TerminalKeyAction::Ignore;
    }

    let input = key_input(event);

    encode_terminal_input(
        &input,
        modifiers_state(event.modifiers),
        KeyEncodeFlags::empty(),
        fallback_text(event),
    )
    .map(TerminalKeyAction::Write)
    .or_else(|| legacy_ctrl_byte(event).map(|b| TerminalKeyAction::Write(vec![b])))
    .unwrap_or(TerminalKeyAction::Ignore)
}

/// Legacy (non-kitty) Ctrl-chord byte (Ctrl-C → 0x03, …). The platform layer
/// filters control characters out of `key_char`, and the shared encoder only
/// builds Ctrl sequences under the kitty protocol, so without this a plain
/// Ctrl+<letter> press encodes to nothing at all.
fn legacy_ctrl_byte(event: &Keystroke) -> Option<u8> {
    let m = &event.modifiers;

    if !m.control || m.alt || m.platform || m.shift {
        return None;
    }

    let mut chars = event.key.chars();

    let (c, rest) = (chars.next()?, chars.next());

    if rest.is_some() {
        return None;
    }

    match c {
        'a'..='z' => Some(c as u8 - b'a' + 1),
        '@' => Some(0x00),
        '[' => Some(0x1b),
        '\\' => Some(0x1c),
        ']' => Some(0x1d),
        '^' => Some(0x1e),
        '_' => Some(0x1f),
        _ => None,
    }
}

fn key_input(event: &Keystroke) -> KeyInput {
    let logical_key = named_key(&event.key)
        .map(Key::Named)
        .unwrap_or_else(|| Key::Character(event.key.as_str().into()));

    KeyInput {
        logical_key: logical_key.clone(),
        key_without_modifiers: logical_key,
        text_with_all_modifiers: event.key_char.as_deref().map(Into::into),
        location: KeyLocation::Standard,
        state: ElementState::Pressed,
        repeat: false,
    }
}

fn fallback_text(event: &Keystroke) -> Option<&str> {
    if event.key.eq_ignore_ascii_case("enter") {
        return match (event.modifiers.control, event.modifiers.alt) {
            (false, false) => Some("\r"),
            (true, false) => Some("\n"),
            (false, true) => Some("\x1b\r"),
            (true, true) => Some("\x1b\n"),
        };
    }

    event.key_char.as_deref().or(match event.key.as_str() {
        "tab" => Some("\t"),
        "escape" | "esc" => Some("\x1b"),
        "space" => Some(" "),
        _ => None,
    })
}

fn is_clipboard_shortcut(event: &Keystroke, key: &str) -> bool {
    event.modifiers.control
        && !event.modifiers.shift
        && !event.modifiers.alt
        && !event.modifiers.platform
        && event.key.eq_ignore_ascii_case(key)
}

fn is_old_clipboard_shortcut(event: &Keystroke) -> bool {
    event.modifiers.control
        && event.modifiers.shift
        && !event.modifiers.alt
        && !event.modifiers.platform
        && matches!(event.key.to_ascii_lowercase().as_str(), "c" | "v")
}

pub(crate) fn modifiers_state(modifiers: Modifiers) -> ModifiersState {
    let mut out = ModifiersState::empty();

    if modifiers.shift {
        out |= ModifiersState::SHIFT;
    }

    if modifiers.alt {
        out |= ModifiersState::ALT;
    }

    if modifiers.control {
        out |= ModifiersState::CONTROL;
    }

    if modifiers.platform {
        out |= ModifiersState::SUPER;
    }

    out
}

fn named_key(name: &str) -> Option<NamedKey> {
    Some(match name.to_ascii_lowercase().as_str() {
        "enter" => NamedKey::Enter,
        "tab" => NamedKey::Tab,
        "backspace" => NamedKey::Backspace,
        "escape" | "esc" => NamedKey::Escape,
        "left" | "arrowleft" => NamedKey::ArrowLeft,
        "right" | "arrowright" => NamedKey::ArrowRight,
        "up" | "arrowup" => NamedKey::ArrowUp,
        "down" | "arrowdown" => NamedKey::ArrowDown,
        "home" => NamedKey::Home,
        "end" => NamedKey::End,
        "pageup" | "page_up" => NamedKey::PageUp,
        "pagedown" | "page_down" => NamedKey::PageDown,
        "insert" => NamedKey::Insert,
        "delete" => NamedKey::Delete,
        "f1" => NamedKey::F1,
        "f2" => NamedKey::F2,
        "f3" => NamedKey::F3,
        "f4" => NamedKey::F4,
        "f5" => NamedKey::F5,
        "f6" => NamedKey::F6,
        "f7" => NamedKey::F7,
        "f8" => NamedKey::F8,
        "f9" => NamedKey::F9,
        "f10" => NamedKey::F10,
        "f11" => NamedKey::F11,
        "f12" => NamedKey::F12,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use gpui::{Keystroke, Modifiers};

    use super::{key_action, pty_bytes_for_key, should_defer_to_ime};
    use crate::terminal::surface::TerminalKeyAction;

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
        match key_action(&key(name, key_char)) {
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
            pty_bytes_for_key(&modified("enter", Some("\r"), Modifiers::none())).as_deref(),
            Some(&b"\r"[..])
        );
        assert_eq!(
            pty_bytes_for_key(&modified("enter", Some("\r"), Modifiers::control())).as_deref(),
            Some(&b"\n"[..])
        );
        assert_eq!(
            pty_bytes_for_key(&modified("enter", Some("\r"), Modifiers::alt())).as_deref(),
            Some(&b"\x1b\r"[..])
        );
        assert_eq!(
            pty_bytes_for_key(&modified("enter", Some("\r"), ctrl_alt)).as_deref(),
            Some(&b"\x1b\n"[..])
        );
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
            pty_bytes_for_key(&ctrl_left).as_deref(),
            Some(&b"\x1b[1;5D"[..])
        );

        let mut shift_tab = key("tab", None);
        shift_tab.modifiers.shift = true;
        assert_eq!(
            pty_bytes_for_key(&shift_tab).as_deref(),
            Some(&b"\x1b[Z"[..])
        );
    }

    #[test]
    fn plain_ctrl_chords_send_legacy_control_bytes() {
        // The platform strips control characters from `key_char`, so these
        // ride the `legacy_ctrl_byte` fallback.
        for (name, byte) in [("z", 0x1au8), ("d", 0x04), ("\\", 0x1c)] {
            assert_eq!(
                pty_bytes_for_key(&modified(name, None, Modifiers::control())).as_deref(),
                Some(&[byte][..]),
                "ctrl-{name}"
            );
        }
        // Ctrl-Shift chords stay with the UI (copy/paste shortcuts).
        let mut ctrl_shift = Modifiers::control();
        ctrl_shift.shift = true;
        assert_eq!(pty_bytes_for_key(&modified("x", None, ctrl_shift)), None);
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
            key_action(&copy),
            TerminalKeyAction::CopyOrWrite(vec![0x03])
        );
        assert_eq!(key_action(&paste), TerminalKeyAction::Paste);
        assert_eq!(pty_bytes_for_key(&copy), None);
    }

    #[test]
    fn ctrl_shift_copy_paste_are_no_longer_app_shortcuts() {
        let copy = modified("c", Some("c"), Modifiers::control_shift());
        let paste = modified("v", Some("v"), Modifiers::control_shift());

        assert_eq!(key_action(&copy), TerminalKeyAction::Ignore);
        assert_eq!(key_action(&paste), TerminalKeyAction::Ignore);
    }
}
