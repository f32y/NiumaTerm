use gpui::{
    App, Bounds, FontStyle, FontWeight, Pixels, ShapedLine, StrikethroughStyle, TextAlign, TextRun,
    UnderlineStyle, Window, fill, point, px, rgb, size,
};
use nmt_terminal::terminal::square::Wide;

use crate::frame::{TerminalColor, TerminalLine};
use crate::metrics;

pub(crate) fn block_separator_bounds(
    bounds: Bounds<Pixels>,
    y: Pixels,
    thickness: f32,
) -> Bounds<Pixels> {
    let left = bounds.left() - px(metrics::PADDING_PX);
    let right = bounds.right() + px(metrics::PADDING_PX);

    Bounds::new(point(left, y), size(right - left, px(thickness)))
}

/// Shape terminal lines with per-cell forced width, cached by the caller's
/// key — the one shaping path for live-frame rows and frozen block rows.
pub(crate) fn shape_lines<'a>(
    lines: impl Iterator<Item = (u64, &'a TerminalLine)>,
    cell_w: f32,
    window: &mut Window,
) -> Vec<ShapedLine> {
    let style = window.text_style();
    let font_size = style.font_size.to_pixels(window.rem_size());
    let base = style.to_run(0);

    lines
        .map(|(key, line)| {
            let runs = terminal_text_runs(line, &base);
            window.text_system().shape_line_by_hash(
                key,
                line.text().len(),
                font_size,
                &runs,
                Some(px(cell_w)),
                || line.text().clone(),
            )
        })
        .collect()
}

/// Build one styled `TextRun` per foreground run, inheriting font/size from the
/// base run and overriding only the color. Run byte-lengths sum to the row text.
pub(crate) fn terminal_text_runs(line: &TerminalLine, base: &TextRun) -> Vec<TextRun> {
    if line.runs().is_empty() {
        // Blank row: keep the single zero/whitespace run path GPUI already handles.
        let mut run = base.clone();

        run.len = line.text().len();

        return vec![run];
    }

    line.runs()
        .iter()
        .map(|run| {
            let mut text_run = base.clone();

            text_run.len = run.len;
            text_run.color = rgb(run.fg.rgb_u32()).into();

            if run.bold {
                text_run.font.weight = FontWeight::BOLD;
            }

            if run.italic {
                text_run.font.style = FontStyle::Italic;
            }

            if run.underline {
                text_run.underline = Some(UnderlineStyle {
                    thickness: px(1.0),
                    color: None,
                    wavy: false,
                });
            }

            if run.strikethrough {
                text_run.strikethrough = Some(StrikethroughStyle {
                    thickness: px(1.0),
                    color: None,
                });
            }

            text_run
        })
        .collect()
}

/// Paint shaped glyph rows at caller-supplied element-local y offsets — the
/// one glyph-paint convention (left-aligned, no wrap, cell-height lines) for
/// live-frame rows and frozen block rows.
pub(crate) fn paint_glyph_rows<'a>(
    bounds: Bounds<Pixels>,
    rows: impl Iterator<Item = (f32, &'a ShapedLine)>,
    cell_h: f32,
    window: &mut Window,
    cx: &mut App,
) {
    for (y, line) in rows {
        let _ = line.paint(
            point(bounds.left(), bounds.top() + px(y)),
            px(cell_h),
            TextAlign::Left,
            None,
            window,
            cx,
        );
    }
}

/// Paint one line's background color runs at an element-local pixel `y`,
/// merging contiguous cells of equal background into single quads. Shared by
/// the live grid and the frozen block rows.
pub(crate) fn paint_line_backgrounds_at(
    bounds: Bounds<Pixels>,
    line: &TerminalLine,
    y: f32,
    cell_w: f32,
    cell_h: f32,
    window: &mut Window,
) {
    let mut run_start = 0u16;
    let mut run_width = 0u16;
    let mut run_color: Option<TerminalColor> = None;

    let flush = |start: u16, width: u16, color: Option<TerminalColor>, window: &mut Window| {
        let Some(color) = color else { return };

        if width == 0 {
            return;
        }

        window.paint_quad(fill(
            Bounds::new(
                point(
                    bounds.left() + px(start as f32 * cell_w),
                    bounds.top() + px(y),
                ),
                size(px(width as f32 * cell_w), px(cell_h)),
            ),
            rgb(color.rgb_u32()),
        ));
    };
    for cell_data in line.cells() {
        let width: u16 = if cell_data.wide == Wide::Wide { 2 } else { 1 };

        if run_color == cell_data.background && run_start + run_width == cell_data.col {
            run_width += width;
            continue;
        }

        flush(run_start, run_width, run_color, window);

        run_start = cell_data.col;
        run_width = width;
        run_color = cell_data.background;
    }

    flush(run_start, run_width, run_color, window);
}
