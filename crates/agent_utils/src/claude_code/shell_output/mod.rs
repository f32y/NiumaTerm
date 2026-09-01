//! Rendering one background shell as the single transcript item its detail
//! view shows.
//!
//! A backgrounded command holds no conversation. Everything it produced goes
//! to a file the CLI names when it hands the command off, so the detail is one
//! command card whose output is read back from that file each time the view
//! asks for it. Re-reading is what makes a still-running command grow on
//! screen: the file is appended to while the command runs, and the transcript
//! update replaces the card only when the text actually differs.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

use crate::background_task::BackgroundTaskState;
use crate::chat::Item;
use crate::claude_code::tasks::ShellDetail;

#[cfg(test)]
mod tests;

/// How much of the output file to show. A background command can be a server
/// or a watch loop that never stops writing, and the end is the part worth
/// reading, so an oversized file is shown from its tail.
const MAX_OUTPUT_BYTES: u64 = 256 * 1024;

/// The one item a background shell's detail view renders.
pub(crate) fn shell_items(detail: &ShellDetail) -> Vec<Item> {
    let status = match detail.state {
        BackgroundTaskState::Failed => "failed",
        state if state.is_terminal() => "completed",
        _ => "inProgress",
    };

    vec![Item::CommandExecution {
        id: detail.id.clone(),
        command: detail.command.clone().unwrap_or_default(),
        purpose: detail.description.clone(),
        aggregated_output: detail.output_file.as_deref().and_then(read_tail),
        status: Some(status.to_string()),
        exit_code: None,
    }]
}

/// The last [`MAX_OUTPUT_BYTES`] of a file, decoded leniently. Command output
/// is whatever bytes the program wrote, which is not guaranteed to be UTF-8 and
/// is cut mid-character by the tail bound either way, so invalid sequences are
/// replaced rather than failing the read.
fn read_tail(path: &str) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();

    if len > MAX_OUTPUT_BYTES {
        file.seek(SeekFrom::Start(len - MAX_OUTPUT_BYTES)).ok()?;
    }

    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;

    let text = String::from_utf8_lossy(&bytes).into_owned();
    (!text.trim().is_empty()).then_some(text)
}
