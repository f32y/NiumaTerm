#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SurfaceCell {
    pub(crate) col: u16,
    pub(crate) row: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SurfaceScreenCell {
    pub(crate) col: u16,
    pub(crate) row: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SurfaceCellSide {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SurfaceMouseButton {
    Left,
    Middle,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SurfaceMouseEventKind {
    Down,
    Up,
    Move,
}

impl TerminalSurface {
    pub(crate) fn apply_mouse(
        &self,
        cell: SurfaceCell,
        side: SurfaceCellSide,
        button: Option<SurfaceMouseButton>,
        kind: SurfaceMouseEventKind,
        modifiers: ModifiersState,
        selection_type: SelectionType,
    ) -> bool {
        if let Some(mode) = self.app_mouse_mode(modifiers) {
            return match kind {
                SurfaceMouseEventKind::Down | SurfaceMouseEventKind::Up => {
                    let Some(code) = button.and_then(mouse_button_code) else {
                        return false;
                    };

                    self.report_mouse(
                        mode,
                        code,
                        kind == SurfaceMouseEventKind::Down,
                        cell.col,
                        cell.row,
                        modifiers,
                    )
                }
                SurfaceMouseEventKind::Move => {
                    let Some(code) = mouse_motion_code(mode, button) else {
                        return false;
                    };

                    self.report_mouse(mode, code, true, cell.col, cell.row, modifiers)
                }
            };
        }

        if button != Some(SurfaceMouseButton::Left) {
            return false;
        }

        let pos = Pos::new(Line(cell.row as i32), Column(cell.col as usize));

        self.apply_selection_at(self.screen_pos(pos), side, kind, selection_type)
    }

    pub(super) fn modes(&self) -> Mode {
        let bits = self.session.vt_modes.load(Ordering::Relaxed);

        Mode::from_bits_truncate(bits)
    }

    pub(super) fn mouse_mode(&self) -> Option<Mode> {
        let mode = self.modes();

        mode.intersects(Mode::MOUSE_MODE).then_some(mode)
    }

    pub(super) fn app_mouse_mode(&self, modifiers: ModifiersState) -> Option<Mode> {
        if modifiers.shift_key() {
            return None;
        }

        self.mouse_mode()
    }

    pub(super) fn report_mouse(
        &self,
        mode: Mode,
        button: u8,
        pressed: bool,
        col: u16,
        row: u16,
        modifiers: ModifiersState,
    ) -> bool {
        let Some(msg) = encode_mouse_report(
            mode.contains(Mode::SGR_MOUSE),
            button,
            mouse_report_mods(modifiers),
            pressed,
            col,
            row,
        ) else {
            return false;
        };

        self.write_bytes(&msg)
    }
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
use std::sync::atomic::Ordering;

use nmt_input::encode_mouse_report;
use nmt_input::keyboard::ModifiersState;
use nmt_terminal::selection::SelectionType;
use nmt_terminal::terminal::Mode;
use nmt_terminal::terminal::pos::{Column, Line, Pos};

use crate::surface::TerminalSurface;
