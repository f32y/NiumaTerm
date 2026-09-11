use nmt_config::system::NewlineShortcut;
use nmt_input::event::ElementState;
use nmt_input::keyboard::{Key, KeyLocation, ModifiersState, NamedKey};
use nmt_input::{KeyEncodeFlags, KeyInput, encode_terminal_input};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TerminalKeyAction {
    Write(Vec<u8>),
    CopyOrWrite(Vec<u8>),
    Paste,
    Ignore,
}

#[derive(Clone, Copy, Debug)]
pub struct TerminalKey<'a> {
    pub key: &'a str,
    pub key_char: Option<&'a str>,
    pub modifiers: ModifiersState,
    pub function: bool,
}

#[cfg(test)]
pub(crate) fn pty_bytes_for_key(
    event: &TerminalKey<'_>,
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
/// through the host's char/IME commit path. Encoding the same key in its key
/// handler would write the character to the PTY twice.
/// Named keys (Enter, Tab, arrows, …) and modified keys still encode normally.
pub fn should_defer_to_ime(event: &TerminalKey<'_>) -> bool {
    event.key_char.is_some()
        && !event.modifiers.control_key()
        && !event.modifiers.alt_key()
        && !event.modifiers.super_key()
        && named_key(event.key).is_none()
}

pub(crate) fn key_action(
    event: &TerminalKey<'_>,
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
        event.modifiers,
        KeyEncodeFlags::empty(),
        fallback_text(event),
    )
    .map(TerminalKeyAction::Write)
    .or_else(|| legacy_ctrl_byte(event).map(|b| TerminalKeyAction::Write(vec![b])))
    .unwrap_or(TerminalKeyAction::Ignore)
}

fn modified_enter_action(
    event: &TerminalKey<'_>,
    newline_shortcut: NewlineShortcut,
) -> Option<TerminalKeyAction> {
    if !event.key.eq_ignore_ascii_case("enter")
        || event.modifiers.alt_key()
        || event.modifiers.super_key()
    {
        return None;
    }

    let inserts_newline = match (event.modifiers.control_key(), event.modifiers.shift_key()) {
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
fn legacy_ctrl_byte(event: &TerminalKey<'_>) -> Option<u8> {
    let m = &event.modifiers;

    if !m.control_key() || m.alt_key() || m.super_key() || m.shift_key() {
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

fn key_input(event: &TerminalKey<'_>) -> KeyInput {
    let logical_key = named_key(event.key)
        .map(Key::Named)
        .unwrap_or_else(|| Key::Character(event.key.into()));

    KeyInput {
        logical_key: logical_key.clone(),
        key_without_modifiers: logical_key,
        text_with_all_modifiers: event.key_char.map(Into::into),
        location: KeyLocation::Standard,
        state: ElementState::Pressed,
        repeat: false,
    }
}

fn fallback_text<'a>(event: &TerminalKey<'a>) -> Option<&'a str> {
    if event.key.eq_ignore_ascii_case("enter") {
        return match (event.modifiers.control_key(), event.modifiers.alt_key()) {
            (false, false) => Some("\r"),
            (true, false) => Some("\n"),
            (false, true) => Some("\x1b\r"),
            (true, true) => Some("\x1b\n"),
        };
    }

    event.key_char.or(match event.key {
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
fn clipboard_action(event: &TerminalKey<'_>) -> Option<TerminalKeyAction> {
    if !event.modifiers.control_key() || event.modifiers.alt_key() || event.modifiers.super_key() {
        return None;
    }

    match (
        event.modifiers.shift_key(),
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
fn clipboard_action(event: &TerminalKey<'_>) -> Option<TerminalKeyAction> {
    if !event.modifiers.super_key()
        || event.modifiers.control_key()
        || event.modifiers.alt_key()
        || event.modifiers.shift_key()
    {
        return None;
    }

    match event.key.to_ascii_lowercase().as_str() {
        "c" => Some(TerminalKeyAction::CopyOrWrite(Vec::new())),
        "v" => Some(TerminalKeyAction::Paste),
        _ => None,
    }
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

/// Wheel steps have a terminal speed multiplier; smooth scrolling arrives as
/// logical rows after the host converts its device's pixel units.
#[derive(Clone, Copy, Debug)]
pub enum WheelDelta {
    Steps(f32),
    Rows(f32),
}

impl WheelDelta {
    pub fn lines(self) -> i32 {
        let raw = match self {
            Self::Steps(steps) => steps * 3.0,
            Self::Rows(rows) => rows,
        };
        if raw.abs() < 0.5 {
            0
        } else {
            raw.round() as i32
        }
    }
}

#[cfg(test)]
mod tests;
