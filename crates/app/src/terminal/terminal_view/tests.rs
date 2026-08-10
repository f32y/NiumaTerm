use gpui::{Bounds, point, px, size};
use nmt_terminal::ansi::CursorShape;

use crate::terminal::frame::TerminalCursor;
use crate::terminal::metrics;
use crate::terminal::terminal_view::cursor_bounds;

#[test]
fn cursor_bounds_cover_block_beam_and_underline() {
    let bounds = Bounds::new(point(px(10.0), px(20.0)), size(px(100.0), px(100.0)));
    let cell = metrics::CellMetrics {
        width_px: 8.0,
        height_px: 18.0,
    };

    let block = cursor_bounds(
        bounds,
        TerminalCursor {
            col: 2,
            row: 1,
            shape: CursorShape::Block,
            color: (0, 0, 0).into(),
        },
        cell,
        0.0,
    )
    .unwrap();
    assert_eq!(block.origin, point(px(26.0), px(38.0)));
    assert_eq!(block.size, size(px(8.0), px(18.0)));

    let beam = cursor_bounds(
        bounds,
        TerminalCursor {
            col: 2,
            row: 1,
            shape: CursorShape::Beam,
            color: (0, 0, 0).into(),
        },
        cell,
        0.0,
    )
    .unwrap();
    assert_eq!(beam.size.width, px(1.0));
    assert_eq!(beam.size.height, px(18.0));

    let underline = cursor_bounds(
        bounds,
        TerminalCursor {
            col: 2,
            row: 1,
            shape: CursorShape::Underline,
            color: (0, 0, 0).into(),
        },
        cell,
        0.0,
    )
    .unwrap();
    assert_eq!(underline.origin.y, px(55.0));
    assert_eq!(underline.size, size(px(8.0), px(1.0)));
}
