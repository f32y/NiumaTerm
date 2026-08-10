/// DECCKM — application cursor keys.
pub const CURSOR_KEYS: u16 = 1;
/// IRM — insert/replace (ANSI mode 4).
pub const INSERT: u16 = 4 | 0x8000;
/// DECAWM — autowrap / line wrap.
pub const WRAPAROUND: u16 = 7;
/// DECTCEM — cursor visible.
pub const CURSOR_VISIBLE: u16 = 25;
/// DECKPAM — application keypad.
pub const KEYPAD_KEYS: u16 = 66;
pub const MOUSE_NORMAL: u16 = 1000;
pub const MOUSE_BUTTON: u16 = 1002;
pub const MOUSE_ANY: u16 = 1003;
pub const FOCUS_EVENT: u16 = 1004;
pub const MOUSE_UTF8: u16 = 1005;
pub const MOUSE_SGR: u16 = 1006;
pub const MOUSE_ALTERNATE_SCROLL: u16 = 1007;
pub const MOUSE_URXVT: u16 = 1015;
pub const MOUSE_SGR_PIXELS: u16 = 1016;
pub const ALT_SCREEN: u16 = 1049;
pub const BRACKETED_PASTE: u16 = 2004;
/// DEC synchronized output keeps a TUI frame private until its matching reset.
pub const SYNC_OUTPUT: u16 = 2026;
