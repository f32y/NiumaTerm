#[cfg(test)]
#[path = "mouse_tests.rs"]
mod tests;

use std::{mem, ptr};

use libghostty_vt_sys::{
    MODS_ALT, MODS_CTRL, MODS_SHIFT, Mods as VtMods, MouseAction as VtMouseAction,
    MouseButton as VtMouseButton, MouseEncoder as VtMouseEncoder,
    MouseEncoderOption as VtMouseEncoderOption, MouseEncoderSize as VtMouseEncoderSize,
    MouseEvent as VtMouseEvent, MouseFormat as VtMouseFormat, MousePosition as VtMousePosition,
    MouseTrackingMode as VtMouseTrackingMode, ghostty_mouse_encoder_encode,
    ghostty_mouse_encoder_free, ghostty_mouse_encoder_new, ghostty_mouse_encoder_setopt,
    ghostty_mouse_event_clear_button, ghostty_mouse_event_free, ghostty_mouse_event_new,
    ghostty_mouse_event_set_action, ghostty_mouse_event_set_button, ghostty_mouse_event_set_mods,
    ghostty_mouse_event_set_position,
};
use nmt_input::keyboard::ModifiersState;

use crate::ghostty::{Error, Result};
use crate::vt_modes::Mode;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseAction {
    Press,
    Release,
    Motion,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
}

/// Mouse reports written the way the engine's own encoder writes them, so
/// every coordinate format a program can ask for (X10, UTF-8, SGR, URXVT) is
/// honoured instead of only the two a hand-written encoder knew.
///
/// The input path works in cells and reads the terminal's modes from the
/// published mode word rather than from the engine, which lives on the PTY
/// task. The encoder is therefore told the grid as a surface of one-pixel
/// cells, and SGR-pixel reporting, which needs real pixel positions, is not
/// offered.
pub struct MouseReporter {
    encoder: VtMouseEncoder,
    event: VtMouseEvent,
}

// Both handles are owned exclusively by this value and carry no thread
// affinity, so the reporter can move with the session that owns it. It is
// not `Sync`; the session keeps it behind a `RefCell` on its own thread.
unsafe impl Send for MouseReporter {}

impl MouseReporter {
    pub fn new() -> Result<Self> {
        let mut encoder: VtMouseEncoder = ptr::null_mut();

        Error::from_code(unsafe { ghostty_mouse_encoder_new(ptr::null(), &mut encoder) })?;

        let mut event: VtMouseEvent = ptr::null_mut();

        if let Err(error) =
            Error::from_code(unsafe { ghostty_mouse_event_new(ptr::null(), &mut event) })
        {
            unsafe { ghostty_mouse_encoder_free(encoder) };

            return Err(error);
        }

        // Every move is reported as it arrives; the caller already sends one
        // event per cell change.
        let track_last_cell = false;

        unsafe {
            ghostty_mouse_encoder_setopt(
                encoder,
                VtMouseEncoderOption::TRACK_LAST_CELL,
                (&track_last_cell as *const bool).cast(),
            );
        }

        Ok(Self { encoder, event })
    }

    /// The bytes that report this event under `mode`, or `None` when the
    /// mode does not report it or the format cannot express its position.
    #[allow(clippy::too_many_arguments)]
    pub fn encode(
        &mut self,
        mode: Mode,
        action: MouseAction,
        button: Option<MouseButton>,
        modifiers: ModifiersState,
        col: u16,
        row: u16,
        grid: (u16, u16),
    ) -> Option<Vec<u8>> {
        let tracking = if mode.contains(Mode::MOUSE_MOTION) {
            VtMouseTrackingMode::ANY
        } else if mode.contains(Mode::MOUSE_DRAG) {
            VtMouseTrackingMode::BUTTON
        } else if mode.contains(Mode::MOUSE_REPORT_CLICK) {
            VtMouseTrackingMode::NORMAL
        } else {
            return None;
        };

        // A program that enables several formats gets the one xterm prefers.
        let format = if mode.contains(Mode::SGR_MOUSE) {
            VtMouseFormat::SGR
        } else if mode.contains(Mode::URXVT_MOUSE) {
            VtMouseFormat::URXVT
        } else if mode.contains(Mode::UTF8_MOUSE) {
            VtMouseFormat::UTF8
        } else {
            VtMouseFormat::X10
        };

        let (cols, rows) = grid;

        let size = VtMouseEncoderSize {
            size: mem::size_of::<VtMouseEncoderSize>(),
            screen_width: u32::from(cols.max(1)),
            screen_height: u32::from(rows.max(1)),
            cell_width: 1,
            cell_height: 1,
            padding_top: 0,
            padding_bottom: 0,
            padding_right: 0,
            padding_left: 0,
        };

        // Button-event tracking reports a move only while a button is held.
        let any_button_pressed = action == MouseAction::Motion && button.is_some();

        let mut mods: VtMods = 0;

        if modifiers.shift_key() {
            mods |= MODS_SHIFT;
        }

        if modifiers.alt_key() {
            mods |= MODS_ALT;
        }

        if modifiers.control_key() {
            mods |= MODS_CTRL;
        }

        unsafe {
            ghostty_mouse_encoder_setopt(
                self.encoder,
                VtMouseEncoderOption::EVENT,
                (&tracking as *const VtMouseTrackingMode::Type).cast(),
            );

            ghostty_mouse_encoder_setopt(
                self.encoder,
                VtMouseEncoderOption::FORMAT,
                (&format as *const VtMouseFormat::Type).cast(),
            );

            ghostty_mouse_encoder_setopt(
                self.encoder,
                VtMouseEncoderOption::SIZE,
                (&size as *const VtMouseEncoderSize).cast(),
            );

            ghostty_mouse_encoder_setopt(
                self.encoder,
                VtMouseEncoderOption::ANY_BUTTON_PRESSED,
                (&any_button_pressed as *const bool).cast(),
            );

            ghostty_mouse_event_set_action(
                self.event,
                match action {
                    MouseAction::Press => VtMouseAction::PRESS,
                    MouseAction::Release => VtMouseAction::RELEASE,
                    MouseAction::Motion => VtMouseAction::MOTION,
                },
            );

            match button {
                Some(button) => ghostty_mouse_event_set_button(
                    self.event,
                    match button {
                        MouseButton::Left => VtMouseButton::LEFT,
                        MouseButton::Middle => VtMouseButton::MIDDLE,
                        MouseButton::Right => VtMouseButton::RIGHT,
                        MouseButton::WheelUp => VtMouseButton::FOUR,
                        MouseButton::WheelDown => VtMouseButton::FIVE,
                    },
                ),
                None => ghostty_mouse_event_clear_button(self.event),
            }

            ghostty_mouse_event_set_mods(self.event, mods);

            // The middle of the cell, so the encoder's pixel-to-cell mapping
            // lands on it however it rounds.
            ghostty_mouse_event_set_position(
                self.event,
                VtMousePosition {
                    x: f32::from(col) + 0.5,
                    y: f32::from(row) + 0.5,
                },
            );
        }

        // The longest report (SGR with five-digit coordinates) fits easily.
        let mut buf = [0u8; 64];
        let mut written = 0usize;

        let encoded = unsafe {
            ghostty_mouse_encoder_encode(
                self.encoder,
                self.event,
                buf.as_mut_ptr().cast(),
                buf.len(),
                &mut written,
            )
        };

        (Error::from_code(encoded).is_ok() && written > 0).then(|| buf[..written].to_vec())
    }
}

impl Drop for MouseReporter {
    fn drop(&mut self) {
        unsafe {
            ghostty_mouse_event_free(self.event);

            ghostty_mouse_encoder_free(self.encoder);
        }
    }
}
