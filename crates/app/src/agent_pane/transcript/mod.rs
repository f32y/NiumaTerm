mod disclosure_row;
mod format;
mod render;
mod rows;
mod virtual_code;

#[allow(unused_imports)]
pub(super) use crate::agent_pane::transcript::disclosure_row::AgentDisclosureRow;
#[cfg(test)]
use crate::agent_pane::transcript::disclosure_row::{
    AGENT_DISCLOSURE_DETAIL_INSET, AGENT_DISCLOSURE_GAP, AGENT_DISCLOSURE_PADDING,
    AGENT_DISCLOSURE_SLOT,
};
#[cfg(test)]
use crate::agent_pane::transcript::format::{
    COMMAND_EXECUTION_HEADING, interrupted_status_label, worked_status_label, working_status_label,
};
#[allow(unused_imports)]
pub(super) use crate::agent_pane::transcript::format::{
    command_execution_detail, command_execution_heading, compact_token_count,
    compaction_accounting, compaction_label, compaction_row_is_expandable, detect_output_language,
    entry_copy_text, fenced_code_block_as, file_extension_lang, hidden, is_work_row,
    permission_icon, relative_time, should_show_jump_to_latest, strip_read_gutter,
    truncated_user_prompt, working_label,
};
#[allow(unused_imports)]
pub(super) use crate::agent_pane::transcript::rows::{
    Entry, RowSpec, TurnSummary, entry_fingerprint, turn_summary,
};
#[cfg(test)]
use crate::agent_pane::transcript::virtual_code::VIRTUAL_TRANSCRIPT_MAX_SEGMENT_BYTES;
#[allow(unused_imports)]
pub(super) use crate::agent_pane::transcript::virtual_code::{
    TranscriptSourceKey, VirtualTranscriptState, code_transcript_format,
    normalized_virtual_transcript, should_virtualize_transcript, transcript_segments,
    transcript_source_key,
};
#[cfg(test)]
mod tests;

#[cfg(test)]
use crate::agent_pane::*;
