//! The engine's VT mode set packed into one word. The PTY thread reads the
//! modes off the engine after every batch and publishes them through an
//! atomic `u32`, so key and mouse encoding on the input path sees the
//! current modes without a round trip to the engine owner.

use bitflags::bitflags;

use crate::ansi::KeyboardModes;

bitflags! {
     #[derive(Debug, Copy, Clone)]
     pub struct Mode: u32 {
        const NONE                    = 0;
        const SHOW_CURSOR             = 1;
        const APP_CURSOR              = 1 << 1;
        const APP_KEYPAD              = 1 << 2;
        const MOUSE_REPORT_CLICK      = 1 << 3;
        const BRACKETED_PASTE         = 1 << 4;
        const SGR_MOUSE               = 1 << 5;
        const MOUSE_MOTION            = 1 << 6;
        const LINE_WRAP               = 1 << 7;
        const LINE_FEED_NEW_LINE      = 1 << 8;
        const ORIGIN                  = 1 << 9;
        const INSERT                  = 1 << 10;
        const FOCUS_IN_OUT            = 1 << 11;
        const ALT_SCREEN              = 1 << 12;
        const MOUSE_DRAG              = 1 << 13;
        const UTF8_MOUSE              = 1 << 14;
        const ALTERNATE_SCROLL        = 1 << 15;
        const VI                      = 1 << 16;
        const URGENCY_HINTS           = 1 << 17;
        const DISAMBIGUATE_ESC_CODES  = 1 << 18;
        const REPORT_EVENT_TYPES      = 1 << 19;
        const REPORT_ALTERNATE_KEYS   = 1 << 20;
        const REPORT_ALL_KEYS_AS_ESC  = 1 << 21;
        const REPORT_ASSOCIATED_TEXT  = 1 << 22;
        const MOUSE_MODE = Self::MOUSE_REPORT_CLICK.bits() | Self::MOUSE_MOTION.bits() | Self::MOUSE_DRAG.bits();

        const KITTY_KEYBOARD_PROTOCOL = Self::DISAMBIGUATE_ESC_CODES.bits()
                                      | Self::REPORT_EVENT_TYPES.bits()
                                      | Self::REPORT_ALTERNATE_KEYS.bits()
                                      | Self::REPORT_ALL_KEYS_AS_ESC.bits()
                                      | Self::REPORT_ASSOCIATED_TEXT.bits();

        const ANY                    = u32::MAX;

        const SIXEL_DISPLAY             = 1 << 28;
        const SIXEL_PRIV_PALETTE        = 1 << 29;
        const SIXEL_CURSOR_TO_THE_RIGHT = 1 << 31;
    }
}

impl Default for Mode {
    fn default() -> Mode {
        Mode::SHOW_CURSOR
            | Mode::LINE_WRAP
            | Mode::ALTERNATE_SCROLL
            | Mode::URGENCY_HINTS
            | Mode::SIXEL_PRIV_PALETTE
    }
}

impl From<KeyboardModes> for Mode {
    fn from(value: KeyboardModes) -> Self {
        let mut mode = Self::empty();

        mode.set(
            Mode::DISAMBIGUATE_ESC_CODES,
            value.contains(KeyboardModes::DISAMBIGUATE_ESC_CODES),
        );

        mode.set(
            Mode::REPORT_EVENT_TYPES,
            value.contains(KeyboardModes::REPORT_EVENT_TYPES),
        );

        mode.set(
            Mode::REPORT_ALTERNATE_KEYS,
            value.contains(KeyboardModes::REPORT_ALTERNATE_KEYS),
        );

        mode.set(
            Mode::REPORT_ALL_KEYS_AS_ESC,
            value.contains(KeyboardModes::REPORT_ALL_KEYS_AS_ESC),
        );

        mode.set(
            Mode::REPORT_ASSOCIATED_TEXT,
            value.contains(KeyboardModes::REPORT_ASSOCIATED_TEXT),
        );

        mode
    }
}
