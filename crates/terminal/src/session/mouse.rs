use nmt_input::keyboard::ModifiersState;

use crate::terminal::Mode;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SurfaceCell {
    pub col: u16,
    pub row: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SurfaceScreenCell {
    pub col: u16,
    pub row: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SurfaceCellSide {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SurfaceMouseButton {
    Left,
    Middle,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SurfaceMouseEventKind {
    Down,
    Up,
    Move,
}

pub(super) fn mouse_button_code(button: SurfaceMouseButton) -> Option<u8> {
    match button {
        SurfaceMouseButton::Left => Some(0),
        SurfaceMouseButton::Middle => Some(1),
        SurfaceMouseButton::Right => Some(2),
    }
}

pub(super) fn mouse_motion_code(mode: Mode, button: Option<SurfaceMouseButton>) -> Option<u8> {
    let button = button.and_then(mouse_button_code);

    if mode.contains(Mode::MOUSE_MOTION) {
        // DECSET 1003 reports every move; no pressed button uses the X10
        // no-button id 3, while a pressed button keeps its own id.
        Some(32 + button.unwrap_or(3))
    } else if mode.contains(Mode::MOUSE_DRAG) {
        // DECSET 1002 reports moves only while a button is held.
        button.map(|button| 32 + button)
    } else {
        None
    }
}

pub(super) fn mouse_report_mods(modifiers: ModifiersState) -> u8 {
    let mut mods = 0;

    if modifiers.shift_key() {
        mods += 4;
    }

    if modifiers.alt_key() {
        mods += 8;
    }

    if modifiers.control_key() {
        mods += 16;
    }

    mods
}
