use std::time;

use nmt_terminal::block_store::SegmentMeta;

use crate::terminal_tab::block_list;
use crate::terminal_tab::layout::truncate_command;
use crate::terminal_tab::session::InFlightBlock;
use crate::terminal_tab::theme::{
    BLOCK_FAILURE_COLOR, BLOCK_INPUT_COLOR, BLOCK_RUNNING_COLOR, BLOCK_SUCCESS_COLOR,
};

/// Chrome of one visible item: gutter accent and right-aligned header.
/// Element coords (scroll already subtracted); may extend
/// past the visible window — the paint's content mask clips.
#[derive(Clone)]
pub(crate) struct FrozenItemChrome {
    pub top: f32,
    pub bottom: f32,
    pub header_y: f32,

    /// 0xRRGGBB, keyed off the exit code (running/success/failure).
    pub accent: u32,

    /// "cmd · ✓ 1.2s" / "cmd · ✗ 127"; `None` when no command is known.
    pub header: Option<String>,
}

/// Gutter/header accent for a frozen item, keyed off the exit code.
pub(super) fn item_accent(meta: &SegmentMeta) -> u32 {
    match meta.exit_code {
        None => BLOCK_RUNNING_COLOR,
        Some(0) => BLOCK_SUCCESS_COLOR,
        Some(_) => BLOCK_FAILURE_COLOR,
    }
}

/// Header label of a frozen item: truncated command + status/duration.
/// `None` without a command (nothing meaningful to show).
pub(super) fn item_header(meta: &SegmentMeta, labels: &DurationLabels) -> Option<String> {
    let command = meta.command.as_deref()?;
    let ended_at = meta.ended_at?;

    let duration = meta
        .started_at
        .and_then(|started_at| ended_at.duration_since(started_at).ok())
        .map(|duration| format_duration(duration, labels));

    let status = match (meta.exit_code, duration) {
        (Some(0), Some(d)) => format!("✓ {d}"),
        (Some(0), None) => "✓".to_string(),
        (Some(code), Some(d)) => format!("✗ {code} · {d}"),
        (Some(code), None) => format!("✗ {code}"),
        (None, Some(d)) => format!("? · {d}"),
        (None, None) => "?".to_string(),
    };

    Some(format!("{} · {status}", truncate_command(command, 32)))
}

/// Chrome of the live item: a running command uses the running accent, while
/// the idle input region uses the input accent. Headers appear only after the
/// item is finished. `rows == 0` → invisible.
pub(crate) fn live_chrome(rows: usize, cell_h: f32, running: bool) -> Option<FrozenItemChrome> {
    if rows == 0 {
        return None;
    }

    let accent = if running {
        BLOCK_RUNNING_COLOR
    } else {
        BLOCK_INPUT_COLOR
    };

    Some(FrozenItemChrome {
        top: 0.0,
        bottom: rows as f32 * cell_h,
        header_y: 0.0,
        accent,
        header: None,
    })
}

/// `1.2s` / `815ms` / `2m05s` — the header's duration label.
pub(crate) fn format_duration(d: time::Duration, labels: &DurationLabels) -> String {
    let secs = d.as_secs();

    if secs >= 60 {
        labels
            .minutes_seconds
            .replace("{minutes}", &(secs / 60).to_string())
            .replace("{seconds}", &format!("{:02}", secs % 60))
    } else if secs >= 1 {
        labels
            .seconds
            .replace("{seconds}", &format!("{:.1}", d.as_secs_f32()))
    } else {
        labels
            .milliseconds
            .replace("{count}", &d.as_millis().to_string())
    }
}

pub(crate) fn block_list_live_chrome(
    live_rows: usize,
    cell_h: f32,
    in_flight: Option<&InFlightBlock>,
    has_open_prompt: bool,
) -> Option<block_list::FrozenItemChrome> {
    let running = in_flight.is_some();

    if !running && !has_open_prompt {
        return None;
    }

    block_list::live_chrome(live_rows, cell_h, running)
}

pub(crate) fn offset_frozen_chrome(
    mut chrome: block_list::FrozenItemChrome,
    item_top: f32,
) -> block_list::FrozenItemChrome {
    chrome.top += item_top;
    chrome.bottom += item_top;
    chrome.header_y += item_top;

    chrome
}

#[derive(Clone)]
pub(crate) struct DurationLabels {
    pub minutes_seconds: String,
    pub seconds: String,
    pub milliseconds: String,
}

#[cfg(test)]
impl Default for DurationLabels {
    fn default() -> Self {
        Self {
            minutes_seconds: "{minutes}m{seconds}s".into(),
            seconds: "{seconds}s".into(),
            milliseconds: "{count}ms".into(),
        }
    }
}
