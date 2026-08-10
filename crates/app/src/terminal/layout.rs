use crate::terminal::frame::{TerminalFrame, TerminalLine};

pub(crate) fn frame_content_rows(frame: &TerminalFrame) -> usize {
    let lines = frame.lines();

    let mut content_end = 0;

    for (row, line) in lines.iter().enumerate().rev() {
        if terminal_line_has_content(line) {
            content_end = row + 1;
            break;
        }
    }

    if let Some(cursor) = frame.cursor() {
        content_end = content_end.max(cursor.row as usize + 1);
    }

    content_end.min(lines.len())
}

pub(crate) fn bottom_anchor_offsets(
    frame: &TerminalFrame,
    cell_height: f32,
    fixed_bottom: bool,
) -> Vec<f32> {
    if !fixed_bottom {
        return Vec::new();
    }

    let rows = frame.lines().len();

    let slack = rows.saturating_sub(frame_content_rows(frame)) as f32 * cell_height;

    if slack > 0.0 {
        vec![slack; rows]
    } else {
        Vec::new()
    }
}

pub(crate) fn live_frame_text(frame: &TerminalFrame) -> Option<String> {
    let rows = frame_content_rows(frame);

    if rows == 0 {
        return None;
    }

    let mut lines = frame
        .lines()
        .iter()
        .take(rows)
        .map(terminal_line_plain_text)
        .collect::<Vec<_>>();

    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }

    (!lines.is_empty()).then(|| lines.join("\n"))
}

fn terminal_line_plain_text(line: &TerminalLine) -> String {
    line.text().replace('\u{00a0}', " ").trim_end().to_string()
}

fn terminal_line_has_content(line: &TerminalLine) -> bool {
    line.cells()
        .iter()
        .any(|c| !matches!(c.ch, '\0' | ' ' | '\u{00a0}'))
}

/// The pixel y-offset for a viewport row (0 with no gaps / out of range).
pub(crate) fn row_y_offset(offsets: &[f32], row: usize) -> f32 {
    offsets.get(row).copied().unwrap_or(0.0)
}

/// Inverse of the offset mapping: the viewport row under a content-relative
/// pixel y. A pointer inside the gap above a block maps to the block's first row.
pub(crate) fn terminal_row_at_y(y: f32, cell_height: f32, offsets: &[f32]) -> u16 {
    if offsets.is_empty() {
        return (y / cell_height).floor().max(0.0) as u16;
    }

    for (row, off) in offsets.iter().enumerate() {
        if y < (row as f32 + 1.0) * cell_height + off {
            return row as u16;
        }
    }

    offsets.len().saturating_sub(1) as u16
}

/// First `max` chars of the command for the header label.
pub(crate) fn truncate_command(command: &str, max: usize) -> String {
    if command.chars().count() <= max {
        command.to_string()
    } else {
        let head: String = command.chars().take(max.saturating_sub(1)).collect();
        format!("{head}…")
    }
}

#[cfg(test)]
#[test]
fn bottom_anchor_offsets_pin_content_to_the_floor() {
    use nmt_terminal::render_buffer::RenderBuffer;

    let frame = TerminalFrame::from_render_buffer(&RenderBuffer::new(80, 3));

    assert_eq!(
        bottom_anchor_offsets(&frame, 10.0, false),
        Vec::<f32>::new()
    );
    assert_eq!(bottom_anchor_offsets(&frame, 10.0, true), [30.0; 3]);
}
