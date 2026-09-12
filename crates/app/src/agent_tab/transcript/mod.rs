pub use crate::agent_tab::transcript::view::TranscriptView;

pub(super) use nmt_agent::transcript::is_work_item as is_work_row;

pub(super) use crate::agent_tab::transcript::code::CodeTranscriptCache;
pub(super) use crate::agent_tab::transcript::format::{
    LAST_RESPONSE_LIMIT, command_execution_heading, command_failure_reason, compact_token_count,
    compaction_accounting, compaction_label, compaction_row_is_expandable,
    compaction_trigger_label, detect_output_language, entry_copy_text, file_extension_lang, hidden,
    last_response_label, permission_icon, relative_time, should_show_jump_to_latest,
    strip_read_gutter, truncated_user_prompt, working_label,
};
pub(super) use crate::agent_tab::transcript::render::transcript_column;
pub(super) use crate::agent_tab::transcript::rows::{Entry, ReadingPosition, RowSpec};
#[cfg(test)]
pub(super) use crate::agent_tab::transcript::rows::{TurnSummary, turn_summary};
pub(super) use crate::agent_tab::transcript::view::TranscriptAttribution;

mod code;
mod disclosure_row;
mod format;
mod incremental;

mod render;
mod reveal;
mod rows;
mod view;

#[cfg(test)]
#[cfg(all(test, not(enable_profiling)))]
#[path = "profiling/disabled_tests.rs"]
mod profiling_disabled_tests;
#[cfg(test)]
#[cfg(all(test, enable_profiling))]
#[path = "profiling/tests.rs"]
mod profiling_tests;

#[cfg(test)]
mod tests;

#[cfg(test)]
use crate::agent_tab::transcript::code::VIRTUAL_TRANSCRIPT_MAX_SEGMENT_BYTES;
#[cfg(test)]
use crate::agent_tab::transcript::code::should_virtualize_transcript;
#[cfg(test)]
use crate::agent_tab::transcript::code::transcript_segments;
#[cfg(test)]
use crate::agent_tab::transcript::disclosure_row::{
    AGENT_CARD_GAP, AGENT_CARD_ICON_BLOCK, AGENT_CARD_PADDING_X, AGENT_DISCLOSURE_DETAIL_INSET,
};
#[cfg(test)]
use crate::agent_tab::transcript::format::{
    command_execution_detail, elapsed_label, interrupted_status_label, worked_status_label,
    working_status_label,
};
