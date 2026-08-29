mod disclosure_row;
mod format;
mod render;
mod rows;
mod view;
mod virtual_code;

#[cfg(test)]
use crate::transcript::disclosure_row::{
    AGENT_CARD_GAP, AGENT_CARD_ICON_BLOCK, AGENT_CARD_PADDING_X, AGENT_DISCLOSURE_DETAIL_INSET,
};
pub(super) use crate::transcript::format::{
    LAST_RESPONSE_LIMIT, command_execution_detail, command_execution_heading,
    command_failure_reason, compact_token_count, compaction_accounting, compaction_label,
    compaction_row_is_expandable, compaction_trigger_label, detect_output_language,
    entry_copy_text, fenced_code_block_as, file_extension_lang, hidden, is_work_row,
    last_response_label, permission_icon, relative_time, should_show_jump_to_latest,
    strip_read_gutter, truncated_user_prompt, working_label,
};
#[cfg(test)]
use crate::transcript::format::{
    elapsed_label, interrupted_status_label, worked_status_label, working_status_label,
};
pub(super) use crate::transcript::render::transcript_column_margin;
pub(super) use crate::transcript::rows::{Entry, ReadingPosition, RowSpec};
#[cfg(test)]
pub(super) use crate::transcript::rows::{TurnSummary, turn_summary};
pub use crate::transcript::view::TranscriptView;
#[cfg(test)]
use crate::transcript::virtual_code::VIRTUAL_TRANSCRIPT_MAX_SEGMENT_BYTES;
#[cfg(test)]
pub(super) use crate::transcript::virtual_code::transcript_segments;
pub(super) use crate::transcript::virtual_code::{
    VirtualTranscriptState, code_transcript_format, should_virtualize_transcript,
};
#[cfg(test)]
mod tests;

#[cfg(test)]
use crate::*;
