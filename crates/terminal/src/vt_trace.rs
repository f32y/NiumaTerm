//! Temporary VT-state tracer for the Windows ConPTY drag-resize / scrollback bug
//! (`.scratch/remove-crosswords/HANDOFF-resize-conpty.md`). Every resize / scroll /
//! ConPTY-repaint seam calls [`trace`], which appends a one-line summary to
//! `target/logs/rio-vt-trace.log` and writes the **entire** engine content
//! (viewport snapshot + full screen+scrollback via the formatter) to its own file
//! `target/logs/<seq>-<label>.txt`.
//!
//! Gated on the `RIO_VT_TRACE` env var so normal runs pay nothing (the full-content
//! dump is O(scrollback) and must never run in production). Override the output dir
//! with `RIO_VT_TRACE_DIR`. REMOVE this module once the resize bug is fixed.

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::ghostty::{GhosttyTerminal, SnapshotCell, SnapshotStyle, TerminalSnapshot, Underline};

static SEQ: AtomicU64 = AtomicU64::new(0);

/// `true` when `RIO_VT_TRACE` is set in the environment (checked once).
pub fn enabled() -> bool {
    static EN: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *EN.get_or_init(|| std::env::var_os("RIO_VT_TRACE").is_some())
}

fn log_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("RIO_VT_TRACE_DIR") {
        PathBuf::from(dir)
    } else {
        PathBuf::from("target").join("logs")
    }
}

/// Milliseconds since the UNIX epoch, for ordering log lines across threads.
fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// Reconstruct one viewport row's text from the snapshot cells.
fn row_text(snapshot: &TerminalSnapshot, y: u16) -> String {
    let mut row = vec![' '; snapshot.cols as usize];
    for cell in snapshot.cells.iter().filter(|cell| cell.y == y) {
        if let Some(ch) = cell.text.chars().next() {
            if let Some(slot) = row.get_mut(cell.x as usize) {
                *slot = ch;
            }
        }
    }
    row.into_iter().collect::<String>().trim_end().to_string()
}

/// Viewport geometry for measuring ghostty↔ConPTY resize divergence:
/// `(last_nonblank_row, trailing_blank_rows, cursor_from_bottom)`, all in
/// viewport rows. ConPTY positions the prompt relative to ITS viewport; comparing
/// ghostty's cursor-from-bottom against ConPTY's CUP row (`rows - cup_row`) reveals
/// how many rows the two engines disagree by after a resize — the root of the
/// repaint-lands-on-wrong-row corruption.
fn viewport_geometry(s: &TerminalSnapshot) -> (i32, i32, i32) {
    let mut last_nonblank: i32 = -1;
    for cell in &s.cells {
        if !cell.text.trim().is_empty() {
            last_nonblank = last_nonblank.max(cell.y as i32);
        }
    }
    let rows = s.rows as i32;
    let trailing_blank = (rows - 1 - last_nonblank).max(0);
    let cursor_from_bottom = rows - 1 - s.cursor.y as i32;
    (last_nonblank, trailing_blank, cursor_from_bottom)
}

/// Per-row trailing-pad analysis: for each row, the last real glyph column, how
/// many cells exist past it (ConPTY's full-width space padding), and whether any of
/// those trailing cells carry styling (bg/fg/attrs). This answers whether the
/// reflow doubling can be fixed by trimming trailing blanks (safe iff unstyled).
fn trailing_pad_report(s: &TerminalSnapshot) -> String {
    let mut out = String::new();
    let styled = |st: &SnapshotStyle| {
        st.bg.is_some()
            || st.fg.is_some()
            || st.bold
            || st.italic
            || st.faint
            || st.inverse
            || st.strikethrough
            || st.overline
            || st.underline != Underline::None
    };
    for y in 0..s.rows {
        let mut row: Vec<&SnapshotCell> = s.cells.iter().filter(|c| c.y == y).collect();
        row.sort_by_key(|c| c.x);
        // Last column holding a real (non-space) glyph.
        let content_end = row
            .iter()
            .rev()
            .find(|c| !c.text.trim().is_empty())
            .map(|c| c.x as i32)
            .unwrap_or(-1);
        let trailing: Vec<&&SnapshotCell> =
            row.iter().filter(|c| c.x as i32 > content_end).collect();
        if trailing.is_empty() {
            continue;
        }
        let styled_count = trailing.iter().filter(|c| styled(&c.style)).count();
        let max_x = row.iter().map(|c| c.x).max().unwrap_or(0);
        let first_bg = trailing
            .first()
            .map(|c| format!("{:?}", c.style.bg))
            .unwrap_or_default();
        let first_text = trailing
            .first()
            .map(|c| {
                if c.text.is_empty() {
                    "<empty>".to_string()
                } else {
                    format!("{:?}", c.text)
                }
            })
            .unwrap_or_default();
        let _ = std::fmt::Write::write_fmt(
            &mut out,
            format_args!(
                "y={y:3} content_end={content_end:3} trailing_cells={:3} styled={:3} max_x={max_x:3} first_pad_text={first_text} first_pad_bg={first_bg}\n",
                trailing.len(),
                styled_count
            ),
        );
    }
    out
}

fn append_master(dir: &PathBuf, line: &str) {
    let path = dir.join("rio-vt-trace.log");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = f.write_all(line.as_bytes());
    }
}

/// Emit a trace point: one summary line to the master log + a full content dump to
/// its own file. `label` names the seam (becomes part of the filename), `detail` is
/// free-form context (request dims, intent, rewritten bytes, …). No-op unless
/// `RIO_VT_TRACE` is set. Safe to call with the engine lock held — touches only the
/// engine (snapshot + formatter), never the render buffer or crosswords.
pub fn trace(label: &str, engine: &mut GhosttyTerminal, detail: &str) {
    if !enabled() {
        return;
    }
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let ts = now_ms();
    let dir = log_dir();
    let _ = std::fs::create_dir_all(&dir);

    let snapshot = engine.snapshot();
    let full = engine
        .format_text(None, false, false)
        .unwrap_or_else(|e| format!("<format_text err: {e:?}>"));

    let summary = match &snapshot {
        Ok(s) => {
            let (last_nonblank, trailing_blank, cursor_from_bottom) = viewport_geometry(s);
            format!(
                "[vt-trace] #{seq:06} ts={ts} {label} | {detail} | cols={} rows={} \
                 cursor=({},{} vis={}) sb=(total={} offset={} len={}) \
                 last_nonblank={last_nonblank} trail_blank_rows={trailing_blank} \
                 cur_from_bot={cursor_from_bottom}\n",
                s.cols,
                s.rows,
                s.cursor.x,
                s.cursor.y,
                s.cursor.visible,
                s.scrollbar.total,
                s.scrollbar.offset,
                s.scrollbar.len,
            )
        }
        Err(e) => format!("[vt-trace] #{seq:06} ts={ts} {label} | {detail} | snapshot_err={e:?}\n"),
    };
    append_master(&dir, &summary);

    let safe_label: String = label
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let path = dir.join(format!("{seq:06}-{safe_label}.txt"));

    let mut body = String::new();
    body.push_str(&summary);
    body.push_str("---- detail ----\n");
    body.push_str(detail);
    body.push('\n');
    if let Ok(s) = &snapshot {
        body.push_str("---- viewport (snapshot rows, y|text) ----\n");
        for y in 0..s.rows {
            let _ =
                std::fmt::Write::write_fmt(&mut body, format_args!("{y:3}|{}\n", row_text(s, y)));
        }
        body.push_str("---- trailing-pad style (per row with trailing cells) ----\n");
        body.push_str(&trailing_pad_report(s));
    }
    body.push_str("---- full screen + scrollback (format_text) ----\n");
    body.push_str(&full);
    if !full.ends_with('\n') {
        body.push('\n');
    }
    let _ = std::fs::write(&path, body);
}
