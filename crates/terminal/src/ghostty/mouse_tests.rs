use std::str;

use nmt_input::keyboard::ModifiersState;

use crate::ghostty::mouse::{MouseAction, MouseButton, MouseReporter};
use crate::vt_modes::Mode;

const GRID: (u16, u16) = (400, 300);

fn report(
    mode: Mode,
    action: MouseAction,
    button: Option<MouseButton>,
    modifiers: ModifiersState,
    col: u16,
    row: u16,
) -> Option<Vec<u8>> {
    MouseReporter::new()
        .unwrap()
        .encode(mode, action, button, modifiers, col, row, GRID)
}

fn click(mode: Mode, action: MouseAction, col: u16, row: u16) -> Option<Vec<u8>> {
    report(
        mode,
        action,
        Some(MouseButton::Left),
        ModifiersState::empty(),
        col,
        row,
    )
}

#[test]
fn sgr_reports_press_release_and_wheel_with_modifiers() {
    let sgr = Mode::MOUSE_REPORT_CLICK | Mode::SGR_MOUSE;

    assert_eq!(
        click(sgr, MouseAction::Press, 4, 2).unwrap(),
        b"\x1b[<0;5;3M"
    );
    assert_eq!(
        click(sgr, MouseAction::Release, 4, 2).unwrap(),
        b"\x1b[<0;5;3m"
    );
    assert_eq!(
        report(
            sgr,
            MouseAction::Press,
            Some(MouseButton::WheelUp),
            ModifiersState::CONTROL,
            0,
            0,
        )
        .unwrap(),
        b"\x1b[<80;1;1M"
    );
}

#[test]
fn x10_reports_within_its_range_and_drops_positions_past_it() {
    let x10 = Mode::MOUSE_REPORT_CLICK;

    assert_eq!(click(x10, MouseAction::Press, 0, 0).unwrap(), b"\x1b[M !!");
    assert_eq!(
        click(x10, MouseAction::Release, 0, 0).unwrap(),
        b"\x1b[M#!!"
    );
    assert_eq!(click(x10, MouseAction::Press, 300, 0), None);
}

/// A program that asks for UTF-8 or URXVT coordinates gets clicks past the
/// X10 limit instead of silence.
#[test]
fn extended_coordinate_formats_reach_past_the_x10_limit() {
    let utf8 = click(
        Mode::MOUSE_REPORT_CLICK | Mode::UTF8_MOUSE,
        MouseAction::Press,
        300,
        0,
    )
    .unwrap();

    assert_eq!(&utf8[..4], b"\x1b[M ");
    assert_eq!(
        str::from_utf8(&utf8[4..]).unwrap(),
        format!("{}!", char::from_u32(32 + 301).unwrap())
    );

    assert_eq!(
        click(
            Mode::MOUSE_REPORT_CLICK | Mode::URXVT_MOUSE,
            MouseAction::Press,
            300,
            0,
        )
        .unwrap(),
        b"\x1b[32;301;1M"
    );
}

#[test]
fn motion_is_reported_only_as_the_tracking_mode_allows() {
    let sgr = Mode::SGR_MOUSE;
    let none = ModifiersState::empty();

    assert_eq!(
        report(
            Mode::MOUSE_REPORT_CLICK | sgr,
            MouseAction::Motion,
            Some(MouseButton::Left),
            none,
            1,
            1
        ),
        None
    );
    assert_eq!(
        report(
            Mode::MOUSE_DRAG | sgr,
            MouseAction::Motion,
            None,
            none,
            1,
            1
        ),
        None
    );
    assert_eq!(
        report(
            Mode::MOUSE_DRAG | sgr,
            MouseAction::Motion,
            Some(MouseButton::Left),
            none,
            1,
            1
        )
        .unwrap(),
        b"\x1b[<32;2;2M"
    );
    assert_eq!(
        report(
            Mode::MOUSE_MOTION | sgr,
            MouseAction::Motion,
            None,
            none,
            1,
            1
        )
        .unwrap(),
        b"\x1b[<35;2;2M"
    );
}

#[test]
fn nothing_is_reported_without_a_tracking_mode() {
    assert_eq!(click(Mode::SGR_MOUSE, MouseAction::Press, 0, 0), None);
}

#[test]
fn buttons_and_modifiers_use_the_xterm_codes() {
    let sgr = Mode::MOUSE_REPORT_CLICK | Mode::SGR_MOUSE;

    assert_eq!(
        report(
            sgr,
            MouseAction::Press,
            Some(MouseButton::Middle),
            ModifiersState::SHIFT | ModifiersState::CONTROL,
            0,
            0,
        )
        .unwrap(),
        b"[<21;1;1M"
    );
    assert_eq!(
        report(
            Mode::MOUSE_DRAG | Mode::SGR_MOUSE,
            MouseAction::Motion,
            Some(MouseButton::Right),
            ModifiersState::ALT,
            0,
            0,
        )
        .unwrap(),
        b"[<42;1;1M"
    );
}
