use gpui::{ScrollDelta, point, px};

use crate::metrics;
use crate::view::input::dropped_paths_text;
use crate::view::mouse::terminal_scroll_lines;

#[test]
fn dropped_paths_are_space_delimited_and_paths_with_spaces_are_quoted() {
    assert_eq!(
        dropped_paths_text(&[
            "C:\\src\\main.rs".into(),
            "C:\\My Project\\notes.txt".into(),
        ]),
        "C:\\src\\main.rs \"C:\\My Project\\notes.txt\""
    );
}
#[test]
fn scroll_delta_maps_to_terminal_lines() {
    let cell = metrics::CellMetrics {
        width_px: 8.0,
        height_px: 20.0,
    };

    assert_eq!(
        terminal_scroll_lines(ScrollDelta::Pixels(point(px(0.0), px(60.0))), cell),
        3
    );
    assert_eq!(
        terminal_scroll_lines(ScrollDelta::Lines(point(0.0, -2.0)), cell),
        -6
    );
    assert_eq!(
        terminal_scroll_lines(ScrollDelta::Pixels(point(px(0.0), px(4.0))), cell),
        0
    );
}
