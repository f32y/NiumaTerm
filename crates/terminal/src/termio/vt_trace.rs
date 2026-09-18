//! VT diagnostics for cursor, resize, and scrollback problems. Each PTY chunk
//! records its complete bytes and resulting active cursor row in
//! `target/logs/nmt-vt-trace.log`. Explicit state captures such as resize also
//! write the viewport and history to `target/logs/<seq>-<label>.txt`.
//!
//! Gated on the `NMT_VT_TRACE` env var so normal runs pay nothing (the full-content
//! dump is O(scrollback) and must never run in production). Override the output dir
//! with `NMT_VT_TRACE_DIR`.

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::{env, fmt, sync, thread, time};

use crate::ghostty::GhosttyTerminal;
use crate::grid::Style;
use crate::render_buffer::RenderBuffer;

static SEQ: AtomicU64 = AtomicU64::new(0);

/// `true` when `NMT_VT_TRACE` is set in the environment (checked once).
pub fn enabled() -> bool {
    static EN: sync::OnceLock<bool> = sync::OnceLock::new();

    *EN.get_or_init(|| env::var_os("NMT_VT_TRACE").is_some())
}

fn log_dir() -> PathBuf {
    if let Some(dir) = env::var_os("NMT_VT_TRACE_DIR") {
        dir.into()
    } else {
        let target: PathBuf = "target".into();

        target.join("logs")
    }
}

/// Milliseconds since the UNIX epoch, for ordering log lines across threads.
fn now_ms() -> u128 {
    time::SystemTime::now()
        .duration_since(time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// Reconstruct one viewport row's text from the render cells.
fn row_text(snapshot: &RenderBuffer, y: usize) -> String {
    (0..snapshot.cols())
        .map(|x| match snapshot.cell(x, y).c() {
            '\0' => ' ',
            c => c,
        })
        .collect::<String>()
        .trim_end()
        .to_string()
}

/// Viewport geometry for measuring ghostty↔ConPTY resize divergence:
/// `(last_nonblank_row, trailing_blank_rows, cursor_from_bottom)`, all in
/// viewport rows. ConPTY positions the prompt relative to ITS viewport; comparing
/// ghostty's cursor-from-bottom against ConPTY's CUP row (`rows - cup_row`) reveals
/// how many rows the two engines disagree by after a resize — the root of the
/// repaint-lands-on-wrong-row corruption.
fn viewport_geometry(s: &RenderBuffer) -> (i32, i32, i32) {
    let mut last_nonblank: i32 = -1;

    for y in 0..s.rows() {
        if (0..s.cols()).any(|x| {
            let c = s.cell(x, y).c();

            c != '\0' && !c.is_whitespace()
        }) {
            last_nonblank = y as i32;
        }
    }

    let rows = s.rows() as i32;
    let trailing_blank = (rows - 1 - last_nonblank).max(0);
    let cursor_from_bottom = rows - 1 - s.cursor().row.0;

    (last_nonblank, trailing_blank, cursor_from_bottom)
}

/// Per-row trailing-pad analysis: for each row, the last real glyph column, how
/// many cells exist past it (ConPTY's full-width space padding), and whether any of
/// those trailing cells carry styling (bg/fg/attrs). This answers whether the
/// reflow doubling can be fixed by trimming trailing blanks (safe iff unstyled).
fn trailing_pad_report(s: &RenderBuffer) -> String {
    let mut out = String::new();

    for y in 0..s.rows() {
        let row: Vec<_> = (0..s.cols())
            .filter_map(|x| {
                let cell = s.cell(x, y);

                (cell.c() != '\0').then_some((x, cell))
            })
            .collect();

        // Last column holding a real (non-space) glyph.
        let content_end = row
            .iter()
            .rev()
            .find(|(_, cell)| !cell.c().is_whitespace())
            .map(|(x, _)| *x as i32)
            .unwrap_or(-1);

        let trailing: Vec<_> = row
            .iter()
            .filter(|(x, _)| *x as i32 > content_end)
            .collect();

        if trailing.is_empty() {
            continue;
        }

        let styled_count = trailing
            .iter()
            .filter(|(_, cell)| s.style(cell.style_id()) != Style::default())
            .count();

        let max_x = row.iter().map(|(x, _)| *x).max().unwrap_or(0);

        let first_bg = trailing
            .first()
            .map(|(_, cell)| format!("{:?}", s.style(cell.style_id()).bg))
            .unwrap_or_default();

        let first_text = trailing
            .first()
            .map(|(_, cell)| {
                if cell.c() == '\0' {
                    "<empty>".to_string()
                } else {
                    format!("{:?}", cell.c())
                }
            })
            .unwrap_or_default();

        let _ = fmt::Write::write_fmt(
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

struct TraceRecord {
    line: String,
    dump: Option<(String, String)>,
}

fn submit(record: TraceRecord) {
    static WRITER: sync::OnceLock<Option<SyncSender<TraceRecord>>> = sync::OnceLock::new();

    // Disk writes are blocking work. One dedicated thread keeps the log open
    // and serializes records, instead of hopping to a blocking pool per call.
    let writer = WRITER.get_or_init(|| {
        let (sender, receiver) = mpsc::sync_channel(64);

        thread::Builder::new()
            .name("vt-trace".into())
            .spawn(move || write_records(receiver))
            .ok()
            .map(|_| sender)
    });

    let Some(writer) = writer else {
        return;
    };

    // Diagnostics must not stall terminal I/O or grow without bound when
    // storage is slow. The queue preserves submission order across writes.
    if writer.try_send(record).is_err() {
        tracing::warn!("VT trace queue is full; dropping a diagnostic record");
    }
}

fn write_records(receiver: Receiver<TraceRecord>) {
    let dir = log_dir();

    let _ = fs::create_dir_all(&dir);

    let mut log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("nmt-vt-trace.log"))
        .ok();

    for record in receiver {
        if let Some(log) = &mut log {
            let _ = log.write_all(record.line.as_bytes());
        }

        if let Some((name, body)) = record.dump {
            let _ = fs::write(dir.join(name), body);
        }
    }
}

/// Record complete output across read boundaries without formatting history
/// on every keystroke. Byte count and escaping preserve control sequences and
/// partial UTF-8 characters for diagnosing output after a resize has finished.
pub(crate) fn trace_read(route: usize, engine: &GhosttyTerminal, bytes: &[u8]) {
    if !enabled() {
        return;
    }

    let seq = SEQ.fetch_add(1, Ordering::Relaxed);

    let mut line = format!(
        "[vt-trace] #{seq:06} ts={} pty_read route={route} cols={} rows={} active_row={:?} read_bytes={} bytes=",
        now_ms(),
        engine.cols(),
        engine.rows(),
        engine.active_cursor_row(),
        bytes.len(),
    );

    for &byte in bytes {
        match byte {
            b'\\' => line.push_str("\\\\"),
            0x20..=0x7e => line.push(char::from(byte)),
            _ => {
                let _ = fmt::Write::write_fmt(&mut line, format_args!("\\x{byte:02x}"));
            }
        }
    }

    line.push('\n');

    submit(TraceRecord { line, dump: None });
}

/// Emit a trace point: one summary line to the master log + a full content dump to
/// its own file. `label` names the trace source (becomes part of the filename), `detail` is
/// free-form context (requested dimensions, route, and operation). No-op unless
/// `NMT_VT_TRACE` is set. Captures engine state on the PTY owner thread.
pub fn trace(label: &str, engine: &mut GhosttyTerminal, detail: &str) {
    if !enabled() {
        return;
    }

    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let ts = now_ms();

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
                s.cols(),
                s.rows(),
                s.cursor().col.0,
                s.cursor().row.0,
                s.cursor_visible(),
                s.scrollbar().total,
                s.scrollbar().offset,
                s.scrollbar().len,
            )
        }
        Err(e) => format!("[vt-trace] #{seq:06} ts={ts} {label} | {detail} | snapshot_err={e:?}\n"),
    };

    let safe_label: String = label
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();

    let name = format!("{seq:06}-{safe_label}.txt");

    let mut body = String::new();

    body.push_str(&summary);

    body.push_str("---- detail ----\n");

    body.push_str(detail);

    body.push('\n');

    if let Ok(s) = &snapshot {
        body.push_str("---- viewport (snapshot rows, y|text) ----\n");

        for y in 0..s.rows() {
            let _ = fmt::Write::write_fmt(&mut body, format_args!("{y:3}|{}\n", row_text(s, y)));
        }

        body.push_str("---- trailing-pad style (per row with trailing cells) ----\n");

        body.push_str(&trailing_pad_report(s));
    }

    body.push_str("---- full screen + scrollback (format_text) ----\n");

    body.push_str(&full);

    if !full.ends_with('\n') {
        body.push('\n');
    }

    submit(TraceRecord {
        line: summary,
        dump: Some((name, body)),
    });
}
