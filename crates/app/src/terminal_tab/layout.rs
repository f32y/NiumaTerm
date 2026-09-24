use crate::terminal_tab::frame::TerminalFrame;

pub(super) fn frame_content_rows(frame: &TerminalFrame) -> usize {
    let lines = frame.lines();

    let mut content_end = 0;

    for (row, line) in lines.iter().enumerate().rev() {
        if line
            .cells()
            .iter()
            .any(|c| !matches!(c.ch, '\0' | ' ' | '\u{00a0}'))
        {
            content_end = row + 1;

            break;
        }
    }

    if let Some(row) = frame.layout_cursor_row() {
        content_end = content_end.max(row + 1);
    }

    content_end.min(lines.len())
}

/// The pixel gap above the grid that pins content to the floor in the
/// fixed-bottom input style: the blank rows below the content, as a height.
/// Zero in the waterfall style, where the grid starts at the top.
pub(super) fn bottom_slack(frame: &TerminalFrame, cell_height: f32, fixed_bottom: bool) -> f32 {
    if !fixed_bottom {
        return 0.0;
    }

    let rows = frame.lines().len();

    rows.saturating_sub(frame_content_rows(frame)) as f32 * cell_height
}

/// Inverse of the slack mapping: the viewport row under a content-relative
/// pixel y. A pointer inside the gap above the grid maps to its first row.
pub(super) fn terminal_row_at_y(y: f32, cell_height: f32, slack: f32) -> u16 {
    ((y - slack).max(0.0) / cell_height).floor() as u16
}

/// First `max` chars of the command for the header label.
pub(super) fn truncate_command(command: &str, max: usize) -> String {
    if command.chars().count() <= max {
        command.to_string()
    } else {
        let head: String = command.chars().take(max.saturating_sub(1)).collect();

        format!("{head}…")
    }
}

#[cfg(test)]
#[test]
fn bottom_slack_pins_content_to_the_floor() {
    use nmt_terminal::render_buffer::RenderBuffer;

    let frame = TerminalFrame::from_render_buffer(&RenderBuffer::new(80, 3));

    assert_eq!(bottom_slack(&frame, 10.0, false), 0.0);
    assert_eq!(bottom_slack(&frame, 10.0, true), 30.0);
    assert_eq!(terminal_row_at_y(5.0, 10.0, 30.0), 0);
    assert_eq!(terminal_row_at_y(45.0, 10.0, 30.0), 1);
    assert_eq!(terminal_row_at_y(45.0, 10.0, 0.0), 4);
}
