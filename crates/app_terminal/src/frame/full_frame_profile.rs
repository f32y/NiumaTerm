use std::time::{Duration, Instant};
use std::{fs, hint};

use gpui::{FontRun, Platform, font, px};
use gpui_windows::WindowsPlatform;
use nmt_terminal::ghostty::GhosttyTerminal;
use nmt_terminal::render_buffer::RenderBuffer;

use crate::frame::{GenerationMap, TerminalFrame};

const COLS: u16 = 80;
const ROWS: u16 = 24;
const CELLS_PER_LINE: usize = 72;
const LINES: usize = 20_000;
const FRAMES: usize = 1_000;

/// Distinct 72-column content per line so shaping never hits a cache
/// (worst case: novel program output, e.g. `cat` of a source tree).
fn line_text(i: usize) -> String {
    let body = format!("{i:06} the quick brown fox jumps over the lazy dog {i:x} 0123456789abcdef");
    let mut s: String = body.chars().take(CELLS_PER_LINE).collect();
    while s.chars().count() < CELLS_PER_LINE {
        s.push('.');
    }
    s
}

#[test]
#[ignore = "manual full-frame pipeline profile"]
fn profile_full_frame_pipeline() {
    // 1. parse (write_vt)
    let mut engine = GhosttyTerminal::new(COLS, ROWS, 1_000_000).unwrap();
    let mut vt = String::new();
    let t = Instant::now();
    for i in 0..LINES {
        vt.push_str(&line_text(i));
        vt.push_str("\r\n");
        if vt.len() >= 16 * 1024 {
            engine.write_vt(vt.as_bytes());
            vt.clear();
        }
    }
    if !vt.is_empty() {
        engine.write_vt(vt.as_bytes());
    }
    let parse = t.elapsed();

    // 2 + 3. per-frame snapshot + extract of the live viewport (render thread).
    // A persistent RenderBuffer is reused across frames, as production does.
    let gens = GenerationMap::new();
    let mut render_buf = RenderBuffer::new(COLS as usize, ROWS as usize);
    let mut capture_total = Duration::ZERO;
    let mut extract_total = Duration::ZERO;
    let mut sink = 0usize;
    for _ in 0..FRAMES {
        let s = Instant::now();
        engine.snapshot_into(&mut render_buf).unwrap();
        capture_total += s.elapsed();

        let e = Instant::now();
        let frame = TerminalFrame::from_render_buffer_with_selection(&render_buf, None, &gens);
        extract_total += e.elapsed();
        sink += frame.lines().len();
    }

    // Keep the cursor on row 0 and alternate one cell so every iteration has
    // exactly one content-dirty row while cursor rendering stays unchanged.
    engine.write_vt(b"\x1b[1;1H");
    engine.snapshot_into(&mut render_buf).unwrap();
    let mut previous = TerminalFrame::from_render_buffer_with_selection(&render_buf, None, &gens);
    let mut incremental_total = Duration::ZERO;
    for i in 0..FRAMES {
        engine.write_vt(if i % 2 == 0 { b"\rA" } else { b"\rB" });
        engine.snapshot_into(&mut render_buf).unwrap();
        let e = Instant::now();
        let frame =
            TerminalFrame::from_render_buffer_reusing(&render_buf, None, &gens, Some(&previous));
        incremental_total += e.elapsed();
        sink += frame.lines().len();
        previous = frame;
    }

    // 4. shape novel lines with the real DirectWrite text system (no window).
    let platform = WindowsPlatform::new(false).expect("directwrite platform");
    let pts = platform.text_system();
    let font_id = pts.font_id(&font("Consolas")).expect("Consolas font id");
    let font_size = px(14.0);
    let lines: Vec<String> = (0..LINES).map(line_text).collect();
    let t = Instant::now();
    for text in &lines {
        let runs = [FontRun {
            len: text.len(),
            font_id,
        }];
        let layout = pts.layout_line(text.as_str(), font_size, &runs);
        hint::black_box(&layout);
    }
    let shape = t.elapsed();

    // ---- report, one scale ----
    use std::fmt::Write as _;
    let cells = (LINES * CELLS_PER_LINE) as f64;
    let viewport_cells = (ROWS as usize * COLS as usize) as f64;
    let per_frame_capture = capture_total / FRAMES as u32;
    let per_frame_extract = extract_total / FRAMES as u32;
    let per_frame_incremental = incremental_total / FRAMES as u32;
    let per_frame_shape = Duration::from_secs_f64(shape.as_secs_f64() / LINES as f64 * ROWS as f64);
    let ns_cell = |d: Duration, n: f64| d.as_nanos() as f64 / n;
    let mut report = String::new();
    let _ = writeln!(
        report,
        "full-frame pipeline profile ({LINES} lines x {CELLS_PER_LINE} cells)"
    );
    let _ = writeln!(
        report,
        "  1. parse     total={parse:?}  {:.0} ns/cell  {:.0} lines/s",
        ns_cell(parse, cells),
        LINES as f64 / parse.as_secs_f64()
    );
    let _ = writeln!(
        report,
        "  2. capture   {per_frame_capture:?}/frame  (viewport {ROWS}x{COLS})"
    );
    let _ = writeln!(
        report,
        "  3. extract   {per_frame_extract:?}/frame  {:.0} ns/cell",
        ns_cell(per_frame_extract, viewport_cells)
    );
    let _ = writeln!(
        report,
        "     one-row  {per_frame_incremental:?}/frame  (incremental)"
    );
    let _ = writeln!(
        report,
        "  4. shape     total={shape:?}  {:.0} ns/line  {:.0} ns/cell  {:.0} lines/s",
        shape.as_nanos() as f64 / LINES as f64,
        ns_cell(shape, cells),
        LINES as f64 / shape.as_secs_f64()
    );
    let _ = writeln!(
        report,
        "  => per streamed frame ({ROWS} novel rows): extract {per_frame_extract:?} + shape {per_frame_shape:?}"
    );
    eprint!("{report}");
    let _ = fs::write(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../target/frame_profile.txt"
        ),
        &report,
    );
    assert!(sink > 0);
}
