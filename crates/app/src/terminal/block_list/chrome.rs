use nmt_i18n::i18n;

use crate::terminal::block_list::*;

/// Chrome of one visible item: gutter accent, right-aligned header,
/// selection state. Element coords (scroll already subtracted); may extend
/// past the visible window — the paint's content mask clips.
#[derive(Clone)]
pub(crate) struct FrozenItemChrome {
    pub item: usize,
    pub top: f32,
    pub bottom: f32,
    pub header_y: f32,
    /// 0xRRGGBB, keyed off the exit code (running/success/failure).
    pub accent: u32,
    /// "cmd · ✓ 1.2s" / "cmd · ✗ 127"; `None` when no command is known.
    pub header: Option<String>,
    pub selected: bool,
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
pub(super) fn item_header(meta: &SegmentMeta) -> Option<String> {
    let command = meta.command.as_deref()?;
    let ended_at = meta.ended_at?;
    let duration = meta
        .started_at
        .and_then(|started_at| ended_at.duration_since(started_at).ok())
        .map(format_duration);

    let status = match (meta.exit_code, duration) {
        (Some(0), Some(d)) => format!("✓ {d}"),
        (Some(0), None) => "✓".to_string(),
        (Some(code), Some(d)) => format!("✗ {code} · {d}"),
        (Some(code), None) => format!("✗ {code}"),
        (None, Some(d)) => format!("? · {d}"),
        (None, None) => "?".to_string(),
    };

    Some(command_header(command, &status))
}

fn command_header(command: &str, status: &str) -> String {
    format!("{} · {status}", truncate_command(command, 32))
}

/// Chrome of the live item: a running command uses the running accent, while
/// the idle input region uses the input accent. Headers appear only after the
/// item is finished. `rows == 0` → invisible.
pub(crate) fn live_chrome(
    item: usize,
    rows: usize,
    cell_h: f32,
    running: bool,
    selected: bool,
) -> Option<FrozenItemChrome> {
    if rows == 0 {
        return None;
    }

    let accent = if running {
        BLOCK_RUNNING_COLOR
    } else {
        BLOCK_INPUT_COLOR
    };

    Some(FrozenItemChrome {
        item,
        top: 0.0,
        bottom: rows as f32 * cell_h,
        header_y: 0.0,
        accent,
        header: None,
        selected,
    })
}

/// `1.2s` / `815ms` / `2m05s` — the header's duration label.
pub(crate) fn format_duration(d: time::Duration) -> String {
    let secs = d.as_secs();

    if secs >= 60 {
        i18n("terminal-duration-minutes-seconds")
            .replace("{minutes}", &(secs / 60).to_string())
            .replace("{seconds}", &format!("{:02}", secs % 60))
    } else if secs >= 1 {
        i18n("terminal-duration-seconds").replace("{seconds}", &format!("{:.1}", d.as_secs_f32()))
    } else {
        i18n("terminal-duration-milliseconds").replace("{count}", &d.as_millis().to_string())
    }
}

pub(crate) fn paint_frozen_separators(
    bounds: Bounds<Pixels>,
    separators: &[f32],
    window: &mut Window,
) {
    for y in separators {
        window.paint_quad(fill(
            block_separator_bounds(bounds, bounds.top() + px(*y), 1.0),
            Rgba {
                r: ((SEPARATOR_COLOR >> 16) & 0xff) as f32 / 255.0,
                g: ((SEPARATOR_COLOR >> 8) & 0xff) as f32 / 255.0,
                b: (SEPARATOR_COLOR & 0xff) as f32 / 255.0,
                a: 0.67,
            },
        ));
    }
}

pub(crate) fn paint_frozen_chrome(
    bounds: Bounds<Pixels>,
    items_chrome: &[FrozenItemChrome],
    window: &mut Window,
    cx: &mut App,
) {
    for chrome in items_chrome {
        let top = bounds.top() + px(chrome.top);
        let height = px(chrome.bottom - chrome.top);
        let gutter_alpha = if chrome.selected { 0xe6 } else { 0x59 };

        window.paint_quad(fill(
            Bounds::new(
                point(
                    bounds.left() - px(BLOCK_GUTTER_GAP + BLOCK_GUTTER_WIDTH),
                    top,
                ),
                size(px(BLOCK_GUTTER_WIDTH), height),
            ),
            rgba((chrome.accent << 8) | gutter_alpha),
        ));

        if chrome.selected {
            window.paint_quad(fill(
                Bounds::new(point(bounds.left(), top), size(bounds.size.width, height)),
                rgba(BLOCK_SELECTED_TINT),
            ));
        }
    }

    let style = window.text_style();
    let font_size = style.font_size.to_pixels(window.rem_size());
    for chrome in items_chrome {
        let Some(header) = chrome.header.as_deref() else {
            continue;
        };

        let runs = [TextRun {
            len: header.len(),
            font: style.font(),
            color: Hsla::from(rgb(0x7f8c98)),
            background_color: None,
            underline: None,
            strikethrough: None,
        }];

        let shaped = window.text_system().shape_line(
            SharedString::from(header.to_string()),
            font_size,
            &runs,
            Some(bounds.size.width),
        );

        let _ = shaped.paint(
            point(bounds.left(), bounds.top() + px(chrome.header_y)),
            px(0.0),
            TextAlign::Right,
            Some(bounds.size.width),
            window,
            cx,
        );
    }
}

pub(crate) fn block_list_live_chrome(
    live_index: usize,
    live_rows: usize,
    cell_h: f32,
    in_flight: Option<&InFlightBlock>,
    has_open_prompt: bool,
    selected: bool,
) -> Option<terminal::block_list::FrozenItemChrome> {
    let running = in_flight.is_some();
    if !running && !has_open_prompt {
        return None;
    }
    terminal::block_list::live_chrome(live_index, live_rows, cell_h, running, selected)
}

pub(crate) fn offset_frozen_chrome(
    mut chrome: terminal::block_list::FrozenItemChrome,
    item_top: f32,
) -> terminal::block_list::FrozenItemChrome {
    chrome.top += item_top;
    chrome.bottom += item_top;
    chrome.header_y += item_top;
    chrome
}
