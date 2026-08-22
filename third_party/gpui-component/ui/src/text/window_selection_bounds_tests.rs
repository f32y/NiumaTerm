use gpui::{Bounds, point, px};

use crate::text::window_selection::selected_line_bounds;

#[test]
fn selected_line_bounds_clip_each_part_of_a_multiline_selection() {
    let line = Bounds::from_corners(point(px(10.), px(20.)), point(px(110.), px(40.)));

    assert_eq!(
        selected_line_bounds(line, point(px(30.), px(25.)), point(px(80.), px(35.))),
        Some(Bounds::from_corners(
            point(px(30.), px(20.)),
            point(px(80.), px(40.))
        ))
    );
    assert_eq!(
        selected_line_bounds(line, point(px(70.), px(30.)), point(px(40.), px(80.))),
        Some(Bounds::from_corners(
            point(px(70.), px(20.)),
            point(px(110.), px(40.))
        ))
    );
    assert_eq!(
        selected_line_bounds(line, point(px(30.), px(0.)), point(px(60.), px(30.))),
        Some(Bounds::from_corners(
            point(px(10.), px(20.)),
            point(px(60.), px(40.))
        ))
    );
    assert_eq!(
        selected_line_bounds(line, point(px(30.), px(0.)), point(px(60.), px(80.))),
        Some(line)
    );
    assert_eq!(
        selected_line_bounds(line, point(px(30.), px(50.)), point(px(60.), px(80.))),
        None
    );
}
