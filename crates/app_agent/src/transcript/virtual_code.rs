use std::ops::Range;

pub(super) const VIRTUAL_TRANSCRIPT_MIN_BYTES: usize = 16 * 1024;
pub(super) const VIRTUAL_TRANSCRIPT_MIN_ROWS: usize = 128;
pub(super) const VIRTUAL_TRANSCRIPT_MAX_SEGMENT_BYTES: usize = 4 * 1024;

pub(crate) fn should_virtualize_transcript(text: &str) -> bool {
    text.len() >= VIRTUAL_TRANSCRIPT_MIN_BYTES
        || text.lines().take(VIRTUAL_TRANSCRIPT_MIN_ROWS + 1).count() > VIRTUAL_TRANSCRIPT_MIN_ROWS
}

pub(crate) fn transcript_segments(text: &str) -> Vec<Range<usize>> {
    let mut segments = Vec::new();
    let mut line_start = 0;

    for line in text.split_inclusive('\n') {
        let mut line_end = line_start + line.len();
        if line.ends_with('\n') {
            line_end -= 1;
            if line_end > line_start && text.as_bytes()[line_end - 1] == b'\r' {
                line_end -= 1;
            }
        }

        if line_start == line_end {
            segments.push(line_start..line_start);
        } else {
            let mut segment_start = line_start;
            while segment_start < line_end {
                let mut segment_end =
                    (segment_start + VIRTUAL_TRANSCRIPT_MAX_SEGMENT_BYTES).min(line_end);
                while !text.is_char_boundary(segment_end) {
                    segment_end -= 1;
                }
                segments.push(segment_start..segment_end);
                segment_start = segment_end;
            }
        }

        line_start += line.len();
    }

    if segments.is_empty() && !text.is_empty() {
        segments.push(0..text.len());
    }

    segments
}
