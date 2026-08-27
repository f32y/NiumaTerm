use crate::transcript::{detect_output_language, file_extension_lang, strip_read_gutter};
use crate::*;

pub(super) const VIRTUAL_TRANSCRIPT_MIN_BYTES: usize = 16 * 1024;
pub(super) const VIRTUAL_TRANSCRIPT_MIN_ROWS: usize = 128;
pub(super) const VIRTUAL_TRANSCRIPT_MAX_SEGMENT_BYTES: usize = 4 * 1024;

pub(crate) fn should_virtualize_transcript(code: bool, text: &str) -> bool {
    code && (text.len() >= VIRTUAL_TRANSCRIPT_MIN_BYTES
        || text.lines().take(VIRTUAL_TRANSCRIPT_MIN_ROWS + 1).count() > VIRTUAL_TRANSCRIPT_MIN_ROWS)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct TranscriptSourceKey {
    allocation: usize,
    len: usize,
    edge_hash: u64,
    strip_read_gutter: bool,
}

pub(crate) fn transcript_source_key(text: &str, strip_read_gutter: bool) -> TranscriptSourceKey {
    // Allocation identity and bounded edges catch streamed appends and full
    // payload replacements without rescanning a potentially multi-megabyte
    // transcript whenever unrelated pane state triggers a render.
    let bytes = text.as_bytes();
    let mut edge_hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes.iter().take(64).chain(bytes.iter().rev().take(64)) {
        edge_hash ^= u64::from(*byte);
        edge_hash = edge_hash.wrapping_mul(0x100_0000_01b3);
    }

    TranscriptSourceKey {
        allocation: bytes.as_ptr() as usize,
        len: bytes.len(),
        edge_hash,
        strip_read_gutter,
    }
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

pub(crate) struct VirtualTranscriptState {
    pub(super) source_key: TranscriptSourceKey,
    pub(super) source: SharedString,
    pub(super) segments: Rc<Vec<Range<usize>>>,
    pub(super) widest_segment: usize,
    pub(super) scroll: UniformListScrollHandle,
}

impl VirtualTranscriptState {
    pub(super) fn new(text: &str, strip_gutter: bool) -> Self {
        let source_key = transcript_source_key(text, strip_gutter);
        let source = normalized_virtual_transcript(text, strip_gutter);
        let segments = transcript_segments(&source);
        let widest_segment = segments
            .iter()
            .enumerate()
            .max_by_key(|(_, range)| range.len())
            .map_or(0, |(index, _)| index);

        Self {
            source_key,
            source: SharedString::from(source),
            segments: Rc::new(segments),
            widest_segment,
            scroll: UniformListScrollHandle::default(),
        }
    }

    pub(super) fn sync(&mut self, text: &str, strip_gutter: bool) {
        let source_key = transcript_source_key(text, strip_gutter);
        if self.source_key == source_key {
            return;
        }

        let source = normalized_virtual_transcript(text, strip_gutter);
        let segments = transcript_segments(&source);
        self.widest_segment = segments
            .iter()
            .enumerate()
            .max_by_key(|(_, range)| range.len())
            .map_or(0, |(index, _)| index);
        self.source_key = source_key;
        self.source = SharedString::from(source);
        self.segments = Rc::new(segments);
    }
}

pub(crate) fn normalized_virtual_transcript(text: &str, strip_gutter: bool) -> String {
    if strip_gutter {
        strip_read_gutter(text).unwrap_or_else(|| text.to_owned())
    } else {
        text.to_owned()
    }
}

/// Returns the syntax language and whether a Read gutter needs stripping.
/// `None` identifies Markdown-native transcript details that retain rich text
/// rendering instead of entering the code-oriented virtual list.
pub(crate) fn code_transcript_format(
    item: &SessionItem,
    detail: &str,
) -> Option<(Cow<'static, str>, bool)> {
    match item {
        SessionItem::CommandExecution { .. } => {
            Some((Cow::Borrowed(detect_output_language(detail)), false))
        }
        SessionItem::FileChange { .. } => Some((Cow::Borrowed("diff"), false)),
        SessionItem::Other { kind, title, .. } => match kind.as_str() {
            "TodoWrite" | "ExitPlanMode" | "Task" => None,
            "Read" => Some((Cow::Owned(file_extension_lang(title)), true)),
            _ => Some((Cow::Borrowed(detect_output_language(detail)), false)),
        },
        SessionItem::Reasoning { .. } => None,
        _ => None,
    }
}
