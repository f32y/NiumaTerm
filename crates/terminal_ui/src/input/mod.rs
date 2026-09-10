use gpui::{Keystroke, Modifiers};
use nmt_config::system::NewlineShortcut;
use nmt_input::event::ElementState;
use nmt_input::keyboard::{Key, KeyLocation, ModifiersState, NamedKey};
use nmt_input::{KeyEncodeFlags, KeyInput, encode_terminal_input};

use crate::surface::TerminalKeyAction;

#[cfg(test)]
pub(crate) fn pty_bytes_for_key(
    event: &Keystroke,
    newline_shortcut: NewlineShortcut,
) -> Option<Vec<u8>> {
    match key_action(event, newline_shortcut) {
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

pub(crate) fn key_action(
    event: &Keystroke,
    newline_shortcut: NewlineShortcut,
) -> TerminalKeyAction {
    if let Some(action) = modified_enter_action(event, newline_shortcut) {
        return action;
    }

    if let Some(action) = clipboard_action(event) {
        return action;
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

fn modified_enter_action(
    event: &Keystroke,
    newline_shortcut: NewlineShortcut,
) -> Option<TerminalKeyAction> {
    if !event.key.eq_ignore_ascii_case("enter") || event.modifiers.alt || event.modifiers.platform {
        return None;
    }

    let inserts_newline = match (event.modifiers.control, event.modifiers.shift) {
        (true, false) => newline_shortcut == NewlineShortcut::CtrlEnter,
        (false, true) => newline_shortcut == NewlineShortcut::ShiftEnter,
        _ => return None,
    };

    Some(TerminalKeyAction::Write(vec![if inserts_newline {
        b'\n'
    } else {
        b'\r'
    }]))
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

/// The chords the surface answers itself instead of encoding for the shell.
///
/// Ctrl-C is both the copy chord and the interrupt byte here, so it copies a
/// selection when there is one and sends ETX when there is not. Ctrl-Shift-C
/// and Ctrl-Shift-V were the chords before that: they are swallowed rather than
/// encoded, so the habit of reaching for them does nothing instead of writing
/// an escape sequence into the command line.
#[cfg(not(target_os = "macos"))]
fn clipboard_action(event: &Keystroke) -> Option<TerminalKeyAction> {
    if !event.modifiers.control || event.modifiers.alt || event.modifiers.platform {
        return None;
    }

    match (
        event.modifiers.shift,
        event.key.to_ascii_lowercase().as_str(),
    ) {
        (false, "c") => Some(TerminalKeyAction::CopyOrWrite(vec![0x03])),
        (false, "v") => Some(TerminalKeyAction::Paste),
        (true, "c" | "v") => Some(TerminalKeyAction::Ignore),
        _ => None,
    }
}

/// Command-C and Command-V, which is where macOS puts the clipboard.
///
/// Control keeps its terminal meaning on this platform, so Ctrl-C is the
/// interrupt byte and nothing else. That leaves Command-C with no byte to fall
/// back to, which is why it carries none: with nothing selected it copies
/// nothing rather than interrupting the running program.
#[cfg(target_os = "macos")]
fn clipboard_action(event: &Keystroke) -> Option<TerminalKeyAction> {
    if !event.modifiers.platform
        || event.modifiers.control
        || event.modifiers.alt
        || event.modifiers.shift
    {
        return None;
    }

    match event.key.to_ascii_lowercase().as_str() {
        "c" => Some(TerminalKeyAction::CopyOrWrite(Vec::new())),
        "v" => Some(TerminalKeyAction::Paste),
        _ => None,
    }
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
mod tests;
