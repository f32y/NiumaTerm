//! Native keyboard input model for the terminal key encoder.
//!
//! Copied from winit's keyboard types (winit is MIT/Apache-2.0) so this crate no
//! longer depends on winit — only the variants the encoder and the GPUI frontend
//! actually use. The `Key<Str>` generic + `as_ref()` mirror winit so the encoder
//! can match owned keys (`Key<SmolStr>`) and string literals (`Key<&str>`).

use bitflags::bitflags;
use smol_str::SmolStr;

/// A logical key. `Str` is `SmolStr` for owned keys; [`Key::as_ref`] yields
/// `Key<&str>` for matching character keys against string literals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Key<Str = SmolStr> {
    /// A key with a well-known name (no text).
    Named(NamedKey),
    /// A key that produces character(s).
    Character(Str),
}

impl Key<SmolStr> {
    /// Borrow the key, turning `Character(SmolStr)` into `Character(&str)`.
    pub fn as_ref(&self) -> Key<&str> {
        match self {
            Key::Named(named) => Key::Named(*named),
            Key::Character(s) => Key::Character(s.as_str()),
        }
    }
}

/// Physical location of a key press (distinguishes the numpad for kitty numpad
/// codes and left/right modifier keys).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyLocation {
    Standard,
    Left,
    Right,
    Numpad,
}

/// Named (non-character) keys the encoder recognizes. This is the subset of
/// winit's `NamedKey` the terminal encoder matches on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamedKey {
    Alt,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    ArrowUp,
    AudioVolumeDown,
    AudioVolumeMute,
    AudioVolumeUp,
    Backspace,
    CapsLock,
    ContextMenu,
    Control,
    Delete,
    End,
    Enter,
    Escape,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
    F13,
    F14,
    F15,
    F16,
    F17,
    F18,
    F19,
    F20,
    F21,
    F22,
    F23,
    F24,
    F25,
    F26,
    F27,
    F28,
    F29,
    F30,
    F31,
    F32,
    F33,
    F34,
    F35,
    Home,
    Hyper,
    Insert,
    MediaFastForward,
    MediaPause,
    MediaPlay,
    MediaPlayPause,
    MediaRecord,
    MediaRewind,
    MediaStop,
    MediaTrackNext,
    MediaTrackPrevious,
    Meta,
    NumLock,
    PageDown,
    PageUp,
    Pause,
    PrintScreen,
    ScrollLock,
    Shift,
    Space,
    Super,
    Tab,
}

impl NamedKey {
    /// The textual representation of a named key, matching winit: only the
    /// control/whitespace keys map to text; navigation/function keys return
    /// `None`. The encoder uses this to decide whether to build an escape
    /// sequence (no text) or take the text path.
    pub fn to_text(&self) -> Option<&str> {
        match self {
            NamedKey::Enter => Some("\r"),
            NamedKey::Backspace => Some("\u{8}"),
            NamedKey::Tab => Some("\t"),
            NamedKey::Space => Some(" "),
            NamedKey::Escape => Some("\u{1b}"),
            _ => None,
        }
    }
}

bitflags! {
    /// Keyboard modifier state. Bit values are arbitrary (internal-only) — only
    /// distinctness matters.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ModifiersState: u32 {
        const SHIFT = 1 << 0;
        const CONTROL = 1 << 1;
        const ALT = 1 << 2;
        const SUPER = 1 << 3;
    }
}

impl ModifiersState {
    pub fn shift_key(&self) -> bool {
        self.contains(Self::SHIFT)
    }
    pub fn control_key(&self) -> bool {
        self.contains(Self::CONTROL)
    }
    pub fn alt_key(&self) -> bool {
        self.contains(Self::ALT)
    }
    pub fn super_key(&self) -> bool {
        self.contains(Self::SUPER)
    }
}
