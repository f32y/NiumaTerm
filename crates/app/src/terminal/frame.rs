use std::collections::hash_map::DefaultHasher;
use std::collections::{self};
use std::hash::{Hash, Hasher};
use std::iter;
use std::sync::{self, Arc};

use gpui::SharedString;
use nmt_config::active_colors;
use nmt_config::colors::term::{DIM_FACTOR, List, TermColors};
use nmt_config::colors::{AnsiColor, ColorRgb, NamedColor};
use nmt_terminal::ansi::CursorShape;
use nmt_terminal::ansi::kitty_virtual::{self, IncompletePlacement, PLACEHOLDER, PlaceholderRun};
use nmt_terminal::ghostty::{ScrollbarInfo, SnapshotPlacement};
use nmt_terminal::grid_emit::{RowSelection, row_selection_for};
use nmt_terminal::render_buffer::RenderBuffer;
use nmt_terminal::selection::SelectionRange;
use nmt_terminal::terminal::square::{ContentTag, Square, Wide};
use nmt_terminal::terminal::style::{Style, StyleFlags};

use crate::terminal;

#[derive(Clone, Default)]
pub(crate) struct TerminalFrame {
    lines: Arc<[TerminalLine]>,
    line_states: Arc<[TerminalLineState]>,
    cols: usize,
    cursor: Option<TerminalCursor>,
    scrollbar: ScrollbarInfo,
    /// Paintable Kitty image placements resolved against the session image cache
    /// Empty in the common no-graphics case.
    images: Arc<[FrameImage]>,
}

#[derive(Clone)]
pub(crate) struct TerminalLine(Arc<TerminalLineData>);

struct TerminalLineData {
    text: SharedString,
    text_hash: u64,
    cells: Box<[TerminalCell]>,
    runs: Box<[StyleRun]>,
    #[cfg(test)]
    cursor_col: Option<u16>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct TerminalLineState {
    version: u64,
    selection: Option<RowSelection>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TerminalCursor {
    pub(crate) col: u16,
    pub(crate) row: u16,
    pub(crate) shape: CursorShape,
    pub(crate) color: TerminalColor,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TerminalCell {
    pub(crate) col: u16,
    pub(crate) ch: char,
    pub(crate) style_id: u16,
    pub(crate) background: Option<TerminalColor>,
    pub(crate) wide: Wide,
    pub(crate) extras: Vec<char>,
    pub(crate) has_cursor: bool,
}

pub(crate) type TerminalColor = ColorRgb;

/// A run of consecutive cells sharing one foreground style, in row order.
/// `len` is the UTF-8 byte length this run contributes to the row text, so the
/// runs line up 1:1 with the shaped line's bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StyleRun {
    pub(crate) len: usize,
    pub(crate) fg: TerminalColor,
    pub(crate) bold: bool,
    pub(crate) italic: bool,
    pub(crate) underline: bool,
    pub(crate) strikethrough: bool,
}

#[derive(Default)]
pub(crate) struct TerminalFrameCache {
    frame: Option<TerminalFrame>,
    /// The frame no longer matches the surface and must be rebuilt on the next
    /// render. `frame` is kept: pointer/IME mapping between the invalidation
    /// and the rebuild must keep using what is on screen — mapping against an
    /// empty cache flips the row offsets mid-drag (broken-selection bug).
    stale: bool,
    full_invalidation: bool,
}

type GenerationMap = collections::HashMap<u32, Arc<terminal::graphics::ImageGeneration>>;

impl TerminalFrame {
    #[cfg(test)]
    pub(crate) fn from_render_buffer(buf: &RenderBuffer) -> Self {
        Self::from_render_buffer_with_selection(buf, None, &GenerationMap::new())
    }

    #[cfg(test)]
    pub(crate) fn from_render_buffer_with_selection(
        buf: &RenderBuffer,
        selection: Option<SelectionRange>,
        generations: &GenerationMap,
    ) -> Self {
        Self::from_render_buffer_reusing(buf, selection, generations, None)
    }

    pub(crate) fn from_render_buffer_reusing(
        buf: &RenderBuffer,
        selection: Option<SelectionRange>,
        generations: &GenerationMap,
        previous: Option<&Self>,
    ) -> Self {
        let colors = BackgroundColors::new(buf.colors());
        let cursor = frame_cursor(buf, &colors);
        let reusable = previous.filter(|frame| {
            frame.cols == buf.cols()
                && frame.lines.len() == buf.rows()
                && frame.line_states.len() == buf.rows()
                && buf.row_versions().len() == buf.rows()
        });

        let mut lines = Vec::with_capacity(buf.rows());
        let mut line_states = Vec::with_capacity(buf.rows());

        for row in 0..buf.rows() {
            let state = TerminalLineState {
                version: buf.row_versions().get(row).copied().unwrap_or_default(),
                selection: row_selection_for(selection, row, buf.cols()),
            };

            let row_cursor = cursor_for_row(cursor, row);

            let line = reusable
                .filter(|frame| {
                    frame.line_states[row] == state
                        && cursor_for_row(frame.cursor, row) == row_cursor
                })
                .map_or_else(
                    || extract_row_with_colors(buf, row, row_cursor, &colors, state.selection),
                    |frame| frame.lines[row].clone(),
                );

            lines.push(line);

            line_states.push(state);
        }

        // Reuse one shared empty `Arc` for the common no-image frame so a graphics-free
        // rebuild allocates nothing for `images` (an empty `Vec::into::<Arc<[_]>>()`
        // still allocates the Arc header).
        let images_vec = extract_frame_images(buf, generations);

        let images = if images_vec.is_empty() {
            empty_images()
        } else {
            images_vec.into()
        };

        Self {
            lines: lines.into_boxed_slice().into(),
            line_states: line_states.into_boxed_slice().into(),
            cols: buf.cols(),
            cursor,
            scrollbar: buf.scrollbar(),
            images,
        }
    }

    pub(crate) fn lines(&self) -> &[TerminalLine] {
        &self.lines
    }

    /// Paintable Kitty image placements for this frame.
    pub(crate) fn images(&self) -> &[FrameImage] {
        &self.images
    }

    pub(crate) fn cursor(&self) -> Option<TerminalCursor> {
        self.cursor
    }

    pub(crate) fn scrollbar(&self) -> ScrollbarInfo {
        self.scrollbar
    }
}

impl TerminalLine {
    pub(crate) fn text(&self) -> &SharedString {
        &self.0.text
    }

    pub(crate) fn cells(&self) -> &[TerminalCell] {
        &self.0.cells
    }

    pub(crate) fn runs(&self) -> &[StyleRun] {
        &self.0.runs
    }

    pub(crate) fn text_hash(&self) -> u64 {
        self.0.text_hash
    }

    #[cfg(test)]
    pub(crate) fn cursor_col(&self) -> Option<u16> {
        self.0.cursor_col
    }

    fn new(
        text: String,
        cells: Vec<TerminalCell>,
        runs: Vec<StyleRun>,
        cursor_col: Option<u16>,
    ) -> Self {
        let text_hash = hash_line(&text, &runs);
        let _ = cursor_col;

        Self(Arc::new(TerminalLineData {
            text: text.into(),
            text_hash,
            cells: cells.into_boxed_slice(),
            runs: runs.into_boxed_slice(),
            #[cfg(test)]
            cursor_col,
        }))
    }

    #[cfg(test)]
    fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// Build a display line from pre-resolved parts (block-split frozen history:
/// the cells were harvested with resolved colors, so no RenderBuffer/engine
/// lookup is involved). The hash folds text + runs, so frozen lines hit the
/// shaped-line cache forever.
pub(crate) fn line_from_parts(
    text: String,
    cells: Vec<TerminalCell>,
    runs: Vec<StyleRun>,
) -> TerminalLine {
    TerminalLine::new(text, cells, runs, None)
}

/// Accumulates display cells into a `TerminalLine` — the one display-
/// convention kernel for the live frame extractor and the frozen engine-row
/// builder: appends display text, merges runs of equal style, and gives wide
/// glyphs an NBSP placeholder column (GPUI's force-width layout snaps one
/// glyph per cell, so without it a wide glyph overlaps the next cell).
#[derive(Default)]
pub(crate) struct LineBuilder {
    text: String,
    cells: Vec<TerminalCell>,
    runs: Vec<StyleRun>,
}

impl LineBuilder {
    pub(crate) fn with_capacity(cols: usize) -> Self {
        Self {
            text: String::with_capacity(cols),
            cells: Vec::with_capacity(cols),
            runs: Vec::new(),
        }
    }

    /// Append one cell's display text; `wide` adds the placeholder column,
    /// covered by the same run. `style.len` is ignored — the run length is
    /// the appended byte count, merged into the previous run on equal style.
    pub(crate) fn push_segment(
        &mut self,
        display: impl Iterator<Item = char>,
        style: StyleRun,
        wide: bool,
    ) {
        let start = self.text.len();

        self.text.extend(display);

        if wide {
            self.text.push('\u{00a0}');
        }

        let seg_len = self.text.len() - start;

        match self.runs.last_mut() {
            Some(last)
                if StyleRun {
                    len: last.len,
                    ..style
                } == *last =>
            {
                last.len += seg_len
            }
            _ => self.runs.push(StyleRun {
                len: seg_len,
                ..style
            }),
        }
    }

    /// Record the cell for background/hit lookups. Separate from
    /// `push_segment` because filler columns (gaps between sparse engine
    /// cells) contribute text but no cell.
    pub(crate) fn push_cell(&mut self, cell: TerminalCell) {
        self.cells.push(cell);
    }

    pub(crate) fn finish(self) -> TerminalLine {
        line_from_parts(self.text, self.cells, self.runs)
    }

    fn finish_with_cursor(self, cursor_col: Option<u16>) -> TerminalLine {
        TerminalLine::new(self.text, self.cells, self.runs, cursor_col)
    }
}

/// A paintable Kitty image in a frame. Metrics-independent: it retains the
/// shared image generation by `Arc` (no pixel copy) plus the geometry needed to place
/// it; final pixel geometry is computed at paint from the active cell metrics and grid
/// bounds. Ordinary and virtual placements normalize to this one descriptor.
#[derive(Clone)]
pub(crate) struct FrameImage {
    pub(crate) generation: Arc<terminal::graphics::ImageGeneration>,
    pub(crate) z: i32,
    pub(crate) kind: FrameImageKind,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum FrameImageKind {
    /// Ordinary overlay placement: Ghostty viewport cell position + grid span + sub-
    /// cell offsets, with a normalized source rectangle into the full image.
    Ordinary {
        viewport_col: i32,
        viewport_row: i32,
        grid_cols: u32,
        grid_rows: u32,
        cell_x_offset: u32,
        cell_y_offset: u32,
        source: [f32; 4],
    },
    /// One row-run of a virtual (Unicode-placeholder) placement, resolved against its
    /// `(image_id, placement_id)` metadata. Final geometry uses `compute_run_geometry`
    /// at paint (aspect-fit needs real cell metrics).
    Virtual {
        run: PlaceholderRun,
        placement_cols: u32,
        placement_rows: u32,
        image_w: u32,
        image_h: u32,
        screen_line: usize,
        screen_col: usize,
    },
}

/// The three Kitty protocol paint layers. Preserved from the placement's
/// z-index; paint buckets by this and keeps engine order within a bucket.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ZLayer {
    /// `z < i32::MIN / 2`: below cell backgrounds.
    BelowBackground,
    /// `i32::MIN / 2 <= z < 0`: above backgrounds, below cursor/text.
    BelowText,
    /// `z >= 0`: above cursor/text.
    AboveText,
}

/// One shared empty `Arc<[FrameImage]>` for graphics-free frames — cloning it is a
/// refcount bump, so the common case allocates nothing.
fn empty_images() -> Arc<[FrameImage]> {
    static EMPTY: sync::OnceLock<Arc<[FrameImage]>> = sync::OnceLock::new();

    EMPTY.get_or_init(|| Arc::from(Vec::new())).clone()
}

impl FrameImage {
    /// The image's top viewport row, for computing its row displacement (fixed-bottom
    /// / block-list) before geometry.
    pub(crate) fn top_row(&self) -> i32 {
        match self.kind {
            FrameImageKind::Ordinary { viewport_row, .. } => viewport_row,
            FrameImageKind::Virtual { screen_line, .. } => screen_line as i32,
        }
    }

    /// Pixel destination rectangle `[x, y, w, h]` and normalized source rectangle
    /// `[u0, v0, u1, v1]` for painting this image. `origin_x`/`origin_y` are
    /// the terminal grid's top-left; `row_offset` is the extra y displacement for this
    /// image's top row (`top_row`). Ordinary placements map viewport cells + sub-cell
    /// offsets directly; virtual runs go through `compute_run_geometry` (aspect-fit).
    /// Returns `None` for degenerate geometry (paint skips it).
    pub(crate) fn destination(
        &self,
        cell_w: f32,
        cell_h: f32,
        origin_x: f32,
        origin_y: f32,
        row_offset: f32,
    ) -> Option<([f32; 4], [f32; 4])> {
        match self.kind {
            FrameImageKind::Ordinary {
                viewport_col,
                viewport_row,
                grid_cols,
                grid_rows,
                cell_x_offset,
                cell_y_offset,
                source,
            } => {
                let dx = origin_x + viewport_col as f32 * cell_w + cell_x_offset as f32;
                let dy =
                    origin_y + viewport_row as f32 * cell_h + row_offset + cell_y_offset as f32;
                let dw = grid_cols as f32 * cell_w;
                let dh = grid_rows as f32 * cell_h;

                if dw <= 0.0 || dh <= 0.0 {
                    return None;
                }

                Some(([dx, dy, dw, dh], source))
            }
            FrameImageKind::Virtual {
                run,
                placement_cols,
                placement_rows,
                image_w,
                image_h,
                screen_line,
                screen_col,
            } => {
                // Fold this row's displacement into the origin and paint the single
                // row at screen line 0 of the adjusted origin.
                let oy = origin_y + screen_line as f32 * cell_h + row_offset;
                let g = kitty_virtual::compute_run_geometry(
                    &run,
                    placement_cols,
                    placement_rows,
                    image_w,
                    image_h,
                    cell_w,
                    cell_h,
                    origin_x,
                    oy,
                    0,
                    screen_col,
                )?;

                Some(([g.x, g.y, g.width, g.height], g.source_rect))
            }
        }
    }

    pub(crate) fn z_layer(&self) -> ZLayer {
        if self.z < i32::MIN / 2 {
            ZLayer::BelowBackground
        } else if self.z < 0 {
            ZLayer::BelowText
        } else {
            ZLayer::AboveText
        }
    }
}

/// Build the paintable image descriptors for a frame. Resolves ordinary and
/// virtual placements against the pre-cloned live generation map; a placement whose
/// image is not cached is skipped (a later update wakes a rebuild). Preserves engine
/// placement order, ordinary before virtual. Metrics-independent — no cell sizing here.
pub(crate) fn extract_frame_images(
    buf: &RenderBuffer,
    generations: &collections::HashMap<u32, Arc<terminal::graphics::ImageGeneration>>,
) -> Vec<FrameImage> {
    // No-graphics fast path: with no placements there is nothing to extract, and a
    // row flagged virtual can only resolve against a virtual placement — so skip the
    // per-row placeholder scan entirely (zero cost when Kitty graphics are unused).
    if buf.placements().is_empty() {
        return Vec::new();
    }

    let mut out = Vec::new();

    extract_ordinary_images(buf, generations, &mut out);
    extract_virtual_images(buf, generations, &mut out);

    out
}

fn image_pixel_size(generation: &terminal::graphics::ImageGeneration) -> Option<(u32, u32)> {
    let size = generation.image().size(0);
    let (w, h) = (size.width.0.max(0) as u32, size.height.0.max(0) as u32);

    (w > 0 && h > 0).then_some((w, h))
}

fn extract_ordinary_images(
    buf: &RenderBuffer,
    generations: &collections::HashMap<u32, Arc<terminal::graphics::ImageGeneration>>,
    out: &mut Vec<FrameImage>,
) {
    for placement in buf.placements().iter().filter(|p| !p.is_virtual) {
        let Some(generation) = generations.get(&placement.image_id) else {
            continue; // pixels not cached yet; skip until the update arrives
        };

        let Some((iw, ih)) = image_pixel_size(generation) else {
            continue;
        };

        out.push(FrameImage {
            generation: generation.clone(),
            z: placement.z,
            kind: FrameImageKind::Ordinary {
                viewport_col: placement.viewport_col,
                viewport_row: placement.viewport_row,
                grid_cols: placement.grid_cols,
                grid_rows: placement.grid_rows,
                cell_x_offset: placement.cell_x_offset,
                cell_y_offset: placement.cell_y_offset,
                source: normalized_source_rect(placement, iw as f32, ih as f32),
            },
        });
    }
}

/// Normalize a placement's pixel source rectangle into `[u0, v0, u1, v1]`. A zero-size
/// source (Ghostty reports no explicit crop) maps to the full image.
fn normalized_source_rect(placement: &SnapshotPlacement, image_w: f32, image_h: f32) -> [f32; 4] {
    if placement.source_width == 0 || placement.source_height == 0 {
        return [0.0, 0.0, 1.0, 1.0];
    }

    let x0 = placement.source_x as f32 / image_w;
    let y0 = placement.source_y as f32 / image_h;
    let x1 = (placement.source_x + placement.source_width) as f32 / image_w;
    let y1 = (placement.source_y + placement.source_height) as f32 / image_h;

    [x0, y0, x1.min(1.0), y1.min(1.0)]
}

fn extract_virtual_images(
    buf: &RenderBuffer,
    generations: &collections::HashMap<u32, Arc<terminal::graphics::ImageGeneration>>,
    out: &mut Vec<FrameImage>,
) {
    for row in 0..buf.rows() {
        // Fast path: rows without any placeholder cell are never scanned.
        if !buf.row_has_virtual_placeholder(row) {
            continue;
        }

        let mut current: Option<(IncompletePlacement, usize)> = None;

        for col in 0..buf.cols() {
            let cell = buf.cell(col, row);
            let is_placeholder =
                cell.content_tag() == ContentTag::Codepoint && cell.c() == PLACEHOLDER;

            if !is_placeholder {
                if let Some((run, start)) = current.take() {
                    push_virtual_run(buf, generations, run, start, row, out);
                }

                continue;
            }

            let style = buf.style(cell.style_id());
            let combining = cell
                .extras_id()
                .and_then(|id| buf.extras().get(&id))
                .map(|extras| extras.zerowidth.as_slice())
                .unwrap_or(&[]);

            let inc = IncompletePlacement::from_cell(style.fg, style.underline_color, combining);

            match &mut current {
                Some((cur, _)) if cur.can_append(&inc) => cur.append(),
                _ => {
                    if let Some((run, start)) = current.take() {
                        push_virtual_run(buf, generations, run, start, row, out);
                    }

                    current = Some((inc, col));
                }
            }
        }
        if let Some((run, start)) = current.take() {
            push_virtual_run(buf, generations, run, start, row, out);
        }
    }
}

/// Resolve a completed placeholder run against its `(image_id, placement_id)` virtual
/// placement metadata and cached image, appending a `Virtual` descriptor. Skipped
/// without drawing a marker if either the placement metadata or the image is missing.
fn push_virtual_run(
    buf: &RenderBuffer,
    generations: &collections::HashMap<u32, Arc<terminal::graphics::ImageGeneration>>,
    incomplete: IncompletePlacement,
    start_col: usize,
    row: usize,
    out: &mut Vec<FrameImage>,
) {
    let run = incomplete.complete();

    let Some(placement) = buf
        .placements()
        .iter()
        .find(|p| p.is_virtual && p.image_id == run.image_id && p.placement_id == run.placement_id)
    else {
        return; // no matching placement metadata
    };

    let Some(generation) = generations.get(&run.image_id) else {
        return; // image not cached
    };

    let Some((iw, ih)) = image_pixel_size(generation) else {
        return;
    };

    out.push(FrameImage {
        generation: generation.clone(),
        z: placement.z,
        kind: FrameImageKind::Virtual {
            run,
            placement_cols: placement.grid_cols,
            placement_rows: placement.grid_rows,
            image_w: iw,
            image_h: ih,
            screen_line: row,
            screen_col: start_col,
        },
    });
}

/// The theme's default foreground, for harvested cells with no explicit fg
/// (block-split).
pub(crate) fn theme_default_foreground() -> TerminalColor {
    TerminalColor::from_color_arr(active_colors().foreground)
}

pub(crate) fn theme_default_background() -> TerminalColor {
    TerminalColor::from_color_arr(active_colors().background.0)
}

/// The theme's selection background (block-split frozen selection).
pub(crate) fn theme_selection_background() -> TerminalColor {
    TerminalColor::from_color_arr(active_colors().selection_background)
}

#[cfg(test)]
pub(crate) fn extract_row(
    buf: &RenderBuffer,
    row: usize,
    cursor: Option<TerminalCursor>,
) -> TerminalLine {
    let colors = BackgroundColors::new(buf.colors());
    extract_row_with_colors(buf, row, cursor, &colors, None)
}

fn extract_row_with_colors(
    buf: &RenderBuffer,
    row: usize,
    cursor: Option<TerminalCursor>,
    colors: &BackgroundColors,
    row_selection: Option<RowSelection>,
) -> TerminalLine {
    let mut builder = LineBuilder::with_capacity(buf.cols());

    for col in 0..buf.cols() {
        let cell = buf.cell(col, row);
        let wide = cell.wide();

        if matches!(wide, Wide::Spacer | Wide::LeadingSpacer) {
            continue;
        }

        let is_codepoint = cell.content_tag() == ContentTag::Codepoint;
        let source_ch = if is_codepoint { cell.c() } else { '\0' };
        let cursor_shape = cursor
            .filter(|cursor| cursor.col == col as u16)
            .map(|cursor| cursor.shape);

        let background = if cell_is_selected(row_selection, col as u16) {
            Some(colors.selection_background)
        } else {
            colors.cell_background(buf, cell)
        };

        let extras = if is_codepoint {
            cell.extras_id()
                .and_then(|extras_id| buf.extras().get(&extras_id))
                .map(|extras| extras.zerowidth.clone())
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        let mut style = if is_codepoint {
            let style = buf.style(cell.style_id());
            let flags = style.flags;
            StyleRun {
                len: 0,
                fg: colors.cell_foreground(style),
                bold: flags.contains(StyleFlags::BOLD),
                italic: flags.contains(StyleFlags::ITALIC),
                underline: flags.intersects(StyleFlags::ALL_UNDERLINES),
                strikethrough: flags.contains(StyleFlags::STRIKEOUT),
            }
        } else {
            StyleRun {
                len: 0,
                fg: colors.default_foreground(),
                bold: false,
                italic: false,
                underline: false,
                strikethrough: false,
            }
        };

        if cursor_shape == Some(CursorShape::Block) {
            // An opaque block replaces the cell background, so painting its glyph
            // with that original background preserves inverse-video contrast.
            style.fg = background.unwrap_or_else(|| colors.named(NamedColor::Background));
        }

        builder.push_segment(
            iter::once(display_char(source_ch)).chain(extras.iter().copied()),
            style,
            wide == Wide::Wide,
        );

        builder.push_cell(TerminalCell {
            col: col as u16,
            ch: source_ch,
            style_id: if is_codepoint { cell.style_id() } else { 0 },
            background,
            wide,
            extras,
            has_cursor: cursor_shape.is_some(),
        });
    }

    builder.finish_with_cursor(cursor.map(|cursor| cursor.col))
}

struct BackgroundColors {
    colors: List,
    term_colors: TermColors,
    selection_background: TerminalColor,
}

impl BackgroundColors {
    fn new(term_colors: TermColors) -> Self {
        // Active theme from config (loader resolves the theme/adaptive palette);
        // `term_colors` still overrides per-index via engine OSC 4 changes.
        let colors = active_colors();
        Self {
            colors: List::from(&colors),
            term_colors,
            selection_background: TerminalColor::from_color_arr(colors.selection_background),
        }
    }

    fn cell_background(&self, buf: &RenderBuffer, cell: Square) -> Option<TerminalColor> {
        match cell.content_tag() {
            ContentTag::BgRgb => {
                let (r, g, b) = cell.bg_rgb();
                Some((r, g, b).into())
            }
            ContentTag::BgPalette => Some(self.indexed(cell.bg_palette_index() as usize)),
            ContentTag::Codepoint => {
                let style = buf.style(cell.style_id());
                self.style_background(style)
            }
        }
    }

    fn cell_foreground(&self, style: Style) -> TerminalColor {
        // Inverse swaps fg/bg: the painted text takes the background color.
        if style.flags.contains(StyleFlags::INVERSE) {
            match style.bg {
                AnsiColor::Named(NamedColor::Background) => self.named(NamedColor::Background),
                _ => self.color(&style.bg, style.flags, false),
            }
        } else {
            self.color(&style.fg, style.flags, true)
        }
    }

    fn default_foreground(&self) -> TerminalColor {
        self.named(NamedColor::Foreground)
    }

    fn style_background(&self, style: Style) -> Option<TerminalColor> {
        if style.flags.contains(StyleFlags::INVERSE) {
            Some(self.color(&style.fg, style.flags, true))
        } else {
            match style.bg {
                AnsiColor::Named(NamedColor::Background) => None,
                _ => Some(self.color(&style.bg, style.flags, false)),
            }
        }
    }

    fn color(&self, color: &AnsiColor, flags: StyleFlags, foreground: bool) -> TerminalColor {
        let dim = foreground && flags.contains(StyleFlags::DIM);
        let bold = foreground && flags.contains(StyleFlags::BOLD);

        match color {
            AnsiColor::Named(named) => {
                let named = if foreground && bold && !dim {
                    named.to_light()
                } else if dim {
                    named.to_dim()
                } else {
                    *named
                };
                self.named(named)
            }
            AnsiColor::Spec(rgb) => {
                if dim {
                    TerminalColor::from_color_arr((*rgb * DIM_FACTOR).to_arr())
                } else {
                    (*rgb).into()
                }
            }
            AnsiColor::Indexed(index) => {
                let index = match (foreground, dim, bold, *index) {
                    (true, true, _, 8..=15) => *index as usize - 8,
                    (true, true, _, 0..=7) => NamedColor::DimBlack as usize + *index as usize,
                    (false, false, true, 0..=7) => *index as usize + 8,
                    (false, true, false, 8..=15) => *index as usize - 8,
                    (false, true, false, 0..=7) => NamedColor::DimBlack as usize + *index as usize,
                    _ => *index as usize,
                };

                self.indexed(index)
            }
        }
    }

    fn named(&self, named: NamedColor) -> TerminalColor {
        self.indexed(named as usize)
    }

    fn indexed(&self, index: usize) -> TerminalColor {
        TerminalColor::from_color_arr(self.term_colors[index].unwrap_or(self.colors[index]))
    }
}

fn cell_is_selected(row_selection: Option<RowSelection>, col: u16) -> bool {
    row_selection.is_some_and(|selection| col >= selection.lo && col <= selection.hi)
}

fn frame_cursor(buf: &RenderBuffer, colors: &BackgroundColors) -> Option<TerminalCursor> {
    let cursor = buf.cursor();
    let shape = buf.cursor_shape();
    (buf.cursor_visible() && cursor.row.0 >= 0 && shape != CursorShape::Hidden).then_some(
        TerminalCursor {
            col: cursor.col.0.min(u16::MAX as usize) as u16,
            row: (cursor.row.0 as usize).min(u16::MAX as usize) as u16,
            shape,
            color: colors.named(NamedColor::Cursor),
        },
    )
}

fn cursor_for_row(cursor: Option<TerminalCursor>, row: usize) -> Option<TerminalCursor> {
    cursor.filter(|cursor| cursor.row as usize == row)
}

fn display_char(ch: char) -> char {
    // Kitty virtual-placeholder cells (U+10EEEE) carry image slices, not glyphs;
    // render them as a blank so GPUI never draws a missing-glyph box while the cell
    // keeps its width for image geometry.
    if ch == '\0' || ch == '\t' || ch == ' ' || ch == PLACEHOLDER {
        '\u{00a0}'
    } else {
        ch
    }
}

fn hash_line(text: &str, runs: &[StyleRun]) -> u64 {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);

    // Fold style into the key so a same-text/different-color row invalidates the
    // shaped-line cache (otherwise recolored output would keep stale glyph runs).
    for run in runs {
        run.len.hash(&mut hasher);
        run.fg.r.hash(&mut hasher);
        run.fg.g.hash(&mut hasher);
        run.fg.b.hash(&mut hasher);
        run.bold.hash(&mut hasher);
        run.italic.hash(&mut hasher);
        run.underline.hash(&mut hasher);
        run.strikethrough.hash(&mut hasher);
    }

    hasher.finish()
}

impl TerminalFrameCache {
    /// The last built frame — served even when stale, so consumers between an
    /// invalidation and the next render keep mapping against what is displayed.
    pub(crate) fn current(&self) -> Option<TerminalFrame> {
        self.frame.clone()
    }

    pub(crate) fn needs_rebuild(&self) -> bool {
        self.stale || self.frame.is_none()
    }

    pub(crate) fn rebuild(&mut self, frame: TerminalFrame) {
        self.frame = Some(frame);
        self.stale = false;
        self.full_invalidation = false;
    }

    pub(crate) fn invalidate(&mut self) {
        self.stale = true;
    }

    pub(crate) fn invalidate_full(&mut self) {
        self.stale = true;
        self.full_invalidation = true;
    }

    pub(crate) fn reusable_frame(&self) -> Option<TerminalFrame> {
        (!self.full_invalidation)
            .then(|| self.frame.clone())
            .flatten()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use nmt_config::colors::term::TermColors;
    use nmt_config::colors::{ColorArray, Colors, NamedColor};
    use nmt_terminal::ansi::CursorShape;
    use nmt_terminal::ansi::kitty_virtual::DIACRITICS;
    use nmt_terminal::ghostty::GhosttyTerminal;
    use nmt_terminal::render_buffer::RenderBuffer;
    use nmt_terminal::selection::SelectionRange;
    use nmt_terminal::terminal::pos::{Column, Line, Pos};
    use nmt_terminal::terminal::square::Wide;

    use super::{
        BackgroundColors, FrameImageKind, GenerationMap, TerminalColor, TerminalFrame,
        TerminalFrameCache, ZLayer, cursor_for_row, extract_frame_images, extract_row,
        extract_row_with_colors, frame_cursor, line_from_parts, theme_default_foreground,
    };
    use crate::terminal;

    fn frame_with_line(line: &str) -> TerminalFrame {
        TerminalFrame {
            lines: Arc::from([line_from_parts(line.to_owned(), Vec::new(), Vec::new())]),
            line_states: Arc::from([Default::default()]),
            cols: line.len(),
            cursor: None,
            scrollbar: Default::default(),
            images: Arc::from([]),
        }
    }

    fn first_line(frame: &TerminalFrame) -> &str {
        frame.lines()[0].text().as_ref()
    }

    #[test]
    fn terminal_cursor_color_prefers_runtime_override() {
        let expected = ColorArray::from([0.8, 0.1, 0.2, 1.0]);
        let mut term_colors = TermColors::default();
        term_colors[NamedColor::Cursor] = Some(expected);

        let colors = BackgroundColors::new(term_colors);

        assert_eq!(
            colors.named(NamedColor::Cursor),
            TerminalColor::from_color_arr(expected)
        );
    }

    #[test]
    fn block_cursor_uses_terminal_background_for_glyph() {
        let mut engine = GhosttyTerminal::new(4, 1, 100).unwrap();
        engine.write_vt(b"A\x1b[D");
        let mut buf = RenderBuffer::new(4, 1);
        engine.snapshot_into(&mut buf).unwrap();

        let gray = |value: u8| {
            let value = f32::from(value) / 255.;
            ColorArray::from([value, value, value, 1.])
        };
        let mut term_colors = TermColors::default();
        term_colors[NamedColor::Foreground] = Some(gray(0x29));
        term_colors[NamedColor::Background] = Some(gray(0xe0));
        term_colors[NamedColor::Cursor] = Some(gray(0x38));
        let colors = BackgroundColors::new(term_colors);
        let cursor = frame_cursor(&buf, &colors).unwrap();
        let row = extract_row_with_colors(&buf, 0, Some(cursor), &colors, None);

        assert_eq!(cursor.shape, CursorShape::Block);
        assert_eq!(row.runs()[0].fg, colors.named(NamedColor::Background));
    }

    #[test]
    fn extracted_rows_have_stable_content_hashes() {
        let mut engine = GhosttyTerminal::new(4, 1, 100).unwrap();
        engine.write_vt(b"ab");
        let mut buf = RenderBuffer::new(4, 1);
        engine.snapshot_into(&mut buf).unwrap();

        let first = extract_row(&buf, 0, None);
        let second = extract_row(&buf, 0, None);
        assert_eq!(first.text_hash(), second.text_hash());

        engine.write_vt(b"c");
        engine.snapshot_into(&mut buf).unwrap();
        let changed = extract_row(&buf, 0, None);
        assert_ne!(first.text_hash(), changed.text_hash());
    }

    /// Regression (broken-selection bug): invalidation marks the cache for
    /// rebuild but keeps serving the last frame, so pointer/IME mapping between
    /// a mouse event and the next render still sees the displayed frame instead
    /// of an empty offsets table.
    #[test]
    fn cache_serves_stale_frame_until_rebuilt() {
        let mut cache = TerminalFrameCache::default();
        assert!(cache.needs_rebuild(), "empty cache must rebuild");

        cache.rebuild(frame_with_line("first"));
        assert!(!cache.needs_rebuild());
        assert_eq!(first_line(&cache.current().unwrap()), "first");

        cache.invalidate();
        assert!(cache.needs_rebuild(), "invalidation forces a rebuild");
        assert!(
            cache.reusable_frame().is_some(),
            "ordinary invalidation keeps the frame eligible for line reuse"
        );
        assert_eq!(
            first_line(&cache.current().unwrap()),
            "first",
            "stale frame stays available for pointer mapping"
        );

        cache.rebuild(frame_with_line("second"));
        assert!(!cache.needs_rebuild());
        assert_eq!(first_line(&cache.current().unwrap()), "second");
    }

    #[test]
    fn cache_full_invalidation_retains_frame_but_disables_reuse_once() {
        let mut cache = TerminalFrameCache::default();
        cache.rebuild(frame_with_line("first"));

        cache.invalidate_full();
        assert!(cache.needs_rebuild());
        assert_eq!(first_line(&cache.current().unwrap()), "first");
        assert!(cache.reusable_frame().is_none());

        cache.rebuild(frame_with_line("second"));
        assert!(!cache.needs_rebuild());
        assert_eq!(
            first_line(&cache.reusable_frame().expect("reuse restored")),
            "second"
        );
    }

    #[test]
    fn incremental_extraction_reuses_only_clean_rows() {
        let mut engine = GhosttyTerminal::new(8, 3, 100).unwrap();
        let mut buf = RenderBuffer::new(8, 3);
        engine.write_vt(b"\x1b[2;1H");
        engine.snapshot_into(&mut buf).unwrap();
        let generations = GenerationMap::new();
        let first = TerminalFrame::from_render_buffer_reusing(&buf, None, &generations, None);

        engine.snapshot_into(&mut buf).unwrap();
        let clean =
            TerminalFrame::from_render_buffer_reusing(&buf, None, &generations, Some(&first));
        assert!(
            first
                .lines()
                .iter()
                .zip(clean.lines())
                .all(|(old, new)| old.ptr_eq(new)),
            "clean capture reuses every line"
        );

        engine.write_vt(b"X");
        engine.snapshot_into(&mut buf).unwrap();
        let changed =
            TerminalFrame::from_render_buffer_reusing(&buf, None, &generations, Some(&clean));
        assert!(clean.lines()[0].ptr_eq(&changed.lines()[0]));
        assert!(!clean.lines()[1].ptr_eq(&changed.lines()[1]));
        assert!(clean.lines()[2].ptr_eq(&changed.lines()[2]));

        let forced = TerminalFrame::from_render_buffer_reusing(&buf, None, &generations, None);
        assert!(
            changed
                .lines()
                .iter()
                .zip(forced.lines())
                .all(|(old, new)| !old.ptr_eq(new)),
            "no reusable frame forces full line extraction"
        );
    }

    #[test]
    fn cursor_only_change_rebuilds_affected_row() {
        let mut engine = GhosttyTerminal::new(8, 2, 100).unwrap();
        let mut buf = RenderBuffer::new(8, 2);
        engine.write_vt(b"AB");
        engine.snapshot_into(&mut buf).unwrap();
        let generations = GenerationMap::new();
        let first = TerminalFrame::from_render_buffer_reusing(&buf, None, &generations, None);
        let versions = buf.row_versions().to_vec();

        engine.write_vt(b"\r");
        engine.snapshot_into(&mut buf).unwrap();
        assert_eq!(buf.row_versions(), versions, "CR changes only the cursor");
        let moved =
            TerminalFrame::from_render_buffer_reusing(&buf, None, &generations, Some(&first));

        assert!(!first.lines()[0].ptr_eq(&moved.lines()[0]));
        assert!(first.lines()[1].ptr_eq(&moved.lines()[1]));
    }

    #[test]
    fn selection_changes_rebuild_only_affected_rows() {
        let mut engine = GhosttyTerminal::new(8, 3, 100).unwrap();
        let mut buf = RenderBuffer::new(8, 3);
        engine.write_vt(b"row0\r\nrow1\r\nrow2");
        engine.snapshot_into(&mut buf).unwrap();
        let generations = GenerationMap::new();
        let plain = TerminalFrame::from_render_buffer_reusing(&buf, None, &generations, None);
        let row0 = SelectionRange::new(
            Pos::new(Line(0), Column(0)),
            Pos::new(Line(0), Column(3)),
            false,
        );
        let selected =
            TerminalFrame::from_render_buffer_reusing(&buf, Some(row0), &generations, Some(&plain));
        assert!(!plain.lines()[0].ptr_eq(&selected.lines()[0]));
        assert!(plain.lines()[1].ptr_eq(&selected.lines()[1]));
        assert!(plain.lines()[2].ptr_eq(&selected.lines()[2]));

        let cleared =
            TerminalFrame::from_render_buffer_reusing(&buf, None, &generations, Some(&selected));
        assert!(!selected.lines()[0].ptr_eq(&cleared.lines()[0]));
        assert!(selected.lines()[1].ptr_eq(&cleared.lines()[1]));
        assert!(selected.lines()[2].ptr_eq(&cleared.lines()[2]));
    }

    #[test]
    fn extracts_row_cells_extras_wide_style_and_cursor() {
        let mut engine = GhosttyTerminal::new(8, 1, 100).unwrap();
        engine.write_vt("e\u{0301}中\x1b[1mB\x1b[0m".as_bytes());
        let mut buf = RenderBuffer::new(8, 1);
        engine.snapshot_into(&mut buf).unwrap();
        let frame = TerminalFrame::from_render_buffer(&buf);
        let row = extract_row(&buf, 0, cursor_for_row(frame.cursor(), 0));

        // The wide '中' is followed by a blank placeholder for its second column.
        assert!(row.text().as_ref().starts_with("e\u{0301}中\u{00a0}B"));
        assert_eq!(row.cursor_col(), Some(4));
        assert!(row.cells().iter().any(|cell| cell.has_cursor));

        let e = &row.cells()[0];
        assert_eq!(e.ch, 'e');
        assert_eq!(e.extras, vec!['\u{0301}']);

        let wide = row.cells().iter().find(|cell| cell.ch == '中').unwrap();
        assert_eq!(wide.wide, Wide::Wide);
        assert!(!row.cells().iter().any(|cell| cell.col == 2));

        let bold = row.cells().iter().find(|cell| cell.ch == 'B').unwrap();
        assert_eq!(bold.style_id, buf.cell(bold.col as usize, 0).style_id());
    }

    #[test]
    fn colored_text_yields_distinct_fg_run_and_cache_key() {
        let mut engine = GhosttyTerminal::new(4, 1, 100).unwrap();
        engine.write_vt(b"\x1b[31mAB\x1b[0m");
        let mut buf = RenderBuffer::new(4, 1);
        engine.snapshot_into(&mut buf).unwrap();
        let colored = extract_row(&buf, 0, None);

        let mut plain_engine = GhosttyTerminal::new(4, 1, 100).unwrap();
        plain_engine.write_vt(b"AB");
        let mut plain_buf = RenderBuffer::new(4, 1);
        plain_engine.snapshot_into(&mut plain_buf).unwrap();
        let plain = extract_row(&plain_buf, 0, None);

        // Identical visible text...
        assert_eq!(colored.text(), plain.text());
        // ...but the red run makes the shape-cache key differ (no stale glyph reuse)...
        assert_ne!(colored.text_hash(), plain.text_hash());
        // ...and a distinct foreground run exists for the colored cells.
        let default_fg = plain.runs()[0].fg;
        assert!(colored.runs().iter().any(|run| run.fg != default_fg));
    }

    #[test]
    fn extracts_cell_backgrounds_from_rgb_style() {
        let mut engine = GhosttyTerminal::new(4, 1, 100).unwrap();
        engine.write_vt(b"\x1b[48;2;1;2;3mA");
        let mut buf = RenderBuffer::new(4, 1);
        engine.snapshot_into(&mut buf).unwrap();

        let row = extract_row(&buf, 0, None);

        assert_eq!(row.cells()[0].background, Some((1, 2, 3).into()));
    }

    #[test]
    fn dim_does_not_change_explicit_background() {
        let mut engine = GhosttyTerminal::new(4, 1, 100).unwrap();
        engine.write_vt(b"\x1b[48;2;120;100;80mA\x1b[2mB");
        let mut buf = RenderBuffer::new(4, 1);
        engine.snapshot_into(&mut buf).unwrap();

        let row = extract_row(&buf, 0, None);

        assert_eq!(row.cells()[0].background, row.cells()[1].background);
    }

    #[test]
    fn selection_overlay_uses_selection_background() {
        let mut engine = GhosttyTerminal::new(4, 1, 100).unwrap();
        engine.write_vt(b"abcd");
        let mut buf = RenderBuffer::new(4, 1);
        engine.snapshot_into(&mut buf).unwrap();
        let selection = SelectionRange::new(
            Pos::new(Line(0), Column(1)),
            Pos::new(Line(0), Column(2)),
            false,
        );

        let frame = TerminalFrame::from_render_buffer_with_selection(
            &buf,
            Some(selection),
            &GenerationMap::new(),
        );
        let selected = TerminalColor::from_color_arr(Colors::default().selection_background);
        let cells = frame.lines()[0].cells();

        assert_eq!(cells[0].background, None);
        assert_eq!(cells[1].background, Some(selected));
        assert_eq!(cells[2].background, Some(selected));
        assert_eq!(cells[3].background, None);
    }

    #[test]
    fn wide_char_gets_placeholder_and_runs_cover_text() {
        let mut engine = GhosttyTerminal::new(6, 1, 100).unwrap();
        engine.write_vt("中A".as_bytes());
        let mut buf = RenderBuffer::new(6, 1);
        engine.snapshot_into(&mut buf).unwrap();
        let row = extract_row(&buf, 0, None);

        // The wide glyph is followed by a blank placeholder for its 2nd column.
        assert!(row.text().as_ref().starts_with("中\u{00a0}A"));
        // Force-width layout needs run byte-lengths to sum to the row text length.
        let run_bytes: usize = row.runs().iter().map(|run| run.len).sum();
        assert_eq!(run_bytes, row.text().len());
    }

    #[test]
    fn inverse_swaps_foreground_into_the_painted_background() {
        // Inverse video paints the cell background with what would be the
        // foreground color, so a plain 'A' fg equals the inverse 'A' background.
        let mut plain_engine = GhosttyTerminal::new(4, 1, 100).unwrap();
        plain_engine.write_vt(b"A");
        let mut plain_buf = RenderBuffer::new(4, 1);
        plain_engine.snapshot_into(&mut plain_buf).unwrap();
        let plain = extract_row(&plain_buf, 0, None);

        let mut engine = GhosttyTerminal::new(4, 1, 100).unwrap();
        engine.write_vt(b"\x1b[7mA");
        let mut buf = RenderBuffer::new(4, 1);
        engine.snapshot_into(&mut buf).unwrap();
        let inverse = extract_row(&buf, 0, None);

        assert_eq!(inverse.cells()[0].background, Some(plain.runs()[0].fg));
    }

    #[test]
    fn text_styles_become_distinct_style_runs() {
        let mut engine = GhosttyTerminal::new(8, 1, 100).unwrap();
        // Bold B, italic I, underline U, strikethrough S, each reset between.
        engine.write_vt(b"\x1b[1mB\x1b[0m\x1b[3mI\x1b[0m\x1b[4mU\x1b[0m\x1b[9mS\x1b[0m");
        let mut buf = RenderBuffer::new(8, 1);
        engine.snapshot_into(&mut buf).unwrap();
        let row = extract_row(&buf, 0, None);

        assert!(
            row.runs()
                .iter()
                .any(|r| r.bold && !r.italic && !r.underline && !r.strikethrough)
        );
        assert!(row.runs().iter().any(|r| r.italic && !r.bold));
        assert!(row.runs().iter().any(|r| r.underline && !r.strikethrough));
        assert!(row.runs().iter().any(|r| r.strikethrough && !r.underline));
    }

    #[test]
    fn bold_toggle_changes_shape_cache_key() {
        let mut plain_engine = GhosttyTerminal::new(4, 1, 100).unwrap();
        plain_engine.write_vt(b"A");
        let mut plain_buf = RenderBuffer::new(4, 1);
        plain_engine.snapshot_into(&mut plain_buf).unwrap();
        let plain = extract_row(&plain_buf, 0, None);

        let mut bold_engine = GhosttyTerminal::new(4, 1, 100).unwrap();
        bold_engine.write_vt(b"\x1b[1mA");
        let mut bold_buf = RenderBuffer::new(4, 1);
        bold_engine.snapshot_into(&mut bold_buf).unwrap();
        let bold = extract_row(&bold_buf, 0, None);

        // Same visible text, but bold must not reuse the plain shaped glyphs.
        assert_eq!(plain.text(), bold.text());
        assert_ne!(plain.text_hash(), bold.text_hash());
    }

    #[test]
    fn extracts_cursor_shape_without_mutating_row_text() {
        let mut engine = GhosttyTerminal::new(4, 1, 100).unwrap();
        engine.write_vt(b"\x1b[5 qA\x1b[D");
        let mut buf = RenderBuffer::new(4, 1);
        engine.snapshot_into(&mut buf).unwrap();

        let frame = TerminalFrame::from_render_buffer(&buf);
        let row = &frame.lines()[0];

        assert_eq!(frame.cursor().unwrap().shape, CursorShape::Beam);
        assert!(row.text().as_ref().starts_with("A\u{00a0}"));
        assert_eq!(row.runs()[0].fg, theme_default_foreground());
    }

    // --- Kitty image frame extraction ---

    use crate::terminal::graphics::graphic_to_generation;

    /// Run `vt` through the engine, mirror it into a `RenderBuffer`, and build a live
    /// generation map from the shipped image deltas — the same inputs frame extraction
    /// sees at runtime.
    fn buf_and_generations(cols: u16, rows: u16, vt: &[u8]) -> (RenderBuffer, GenerationMap) {
        let mut engine = GhosttyTerminal::new(cols, rows, 100).unwrap();
        engine.resize(cols, rows, 10, 20).unwrap();
        engine.write_vt(vt);
        let buf = engine.snapshot().unwrap();

        let release: terminal::graphics::ReleaseQueue = Default::default();
        let (pending, _) = engine.take_image_deltas(buf.placements());
        let mut generations = GenerationMap::new();
        for (id, data) in pending {
            if let Some(g) = graphic_to_generation(data, &release) {
                generations.insert(id, g);
            }
        }
        (buf, generations)
    }

    #[test]
    fn extracts_ordinary_placement_with_source_and_z() {
        let (buf, generations) =
            buf_and_generations(20, 5, b"\x1b_Ga=T,f=32,s=1,v=1,i=1,p=9;/wAA/w==\x1b\\");
        let images = extract_frame_images(&buf, &generations);
        assert_eq!(images.len(), 1, "one ordinary image");
        let img = &images[0];
        assert_eq!(img.z_layer(), ZLayer::AboveText, "z=0 paints above text");
        match img.kind {
            FrameImageKind::Ordinary {
                viewport_col,
                viewport_row,
                source,
                ..
            } => {
                assert_eq!((viewport_col, viewport_row), (0, 0));
                assert_eq!(source, [0.0, 0.0, 1.0, 1.0], "full-image source");
            }
            _ => panic!("expected ordinary"),
        }
    }

    #[test]
    fn ordinary_destination_maps_cells_to_pixels() {
        let (buf, generations) =
            buf_and_generations(20, 5, b"\x1b_Ga=T,f=32,s=1,v=1,i=1;/wAA/w==\x1b\\");
        let img = &extract_frame_images(&buf, &generations)[0];
        // cell 10x20, viewport (0,0), no offsets: dest = one cell, full source.
        let (dest, source) = img.destination(10.0, 20.0, 100.0, 50.0, 0.0).unwrap();
        assert_eq!(dest, [100.0, 50.0, 10.0, 20.0]);
        assert_eq!(source, [0.0, 0.0, 1.0, 1.0]);
        // A row displacement (fixed-bottom / block-list) shifts y only.
        let (dest2, _) = img.destination(10.0, 20.0, 100.0, 50.0, 7.0).unwrap();
        assert_eq!(dest2[1], 57.0);
    }

    #[test]
    fn destination_maps_negative_viewport_row_above_origin() {
        // A placement scrolled one row above the viewport top → negative dest y (paint
        // clips it to the content mask).
        let (buf, generations) =
            buf_and_generations(20, 5, b"\x1b_Ga=T,f=32,s=1,v=1,i=1;/wAA/w==\x1b\\");
        let mut img = extract_frame_images(&buf, &generations).remove(0);
        if let FrameImageKind::Ordinary { viewport_row, .. } = &mut img.kind {
            *viewport_row = -1;
        }
        let (dest, _) = img.destination(10.0, 20.0, 0.0, 0.0, 0.0).unwrap();
        assert_eq!(dest[1], -20.0, "one row above the origin");
    }

    #[test]
    fn skips_placement_whose_image_is_not_cached() {
        // Same buffer, but an empty generation map (pixels not yet delivered).
        let (buf, _) = buf_and_generations(20, 5, b"\x1b_Ga=T,f=32,s=1,v=1,i=1;/wAA/w==\x1b\\");
        let images = extract_frame_images(&buf, &GenerationMap::new());
        assert!(images.is_empty(), "uncached image is skipped, not failed");
    }

    #[test]
    fn plain_rows_are_not_scanned_for_placeholders() {
        // No virtual placeholders anywhere: extraction yields no virtual images and the
        // per-row fast path skips every row (no panic, empty result).
        let (buf, generations) = buf_and_generations(8, 2, b"hello");
        assert!(!buf.row_has_virtual_placeholder(0));
        assert!(extract_frame_images(&buf, &generations).is_empty());
    }

    #[test]
    fn extracts_contiguous_virtual_run() {
        // A 2×1 virtual image (id=7, p=3, c=2 r=1) with two contiguous placeholder
        // cells that inherit column from the first → one run of width 2.
        // Placement id 0 (no `p=`, no underline color) so the run's decoded
        // placement id (from underline) matches the placement metadata.
        let d0 = DIACRITICS[0];
        let cell0 = format!("\x1b[38;2;0;0;7m{}{}", '\u{10EEEE}', d0); // row=0,col=0
        let cell1 = format!("{}", '\u{10EEEE}'); // inherit row/col
        let mut vt = Vec::new();
        vt.extend_from_slice(b"\x1b_Ga=T,U=1,f=32,s=2,v=1,i=7,c=2,r=1;/wAA//8AAP8=\x1b\\");
        vt.extend_from_slice(cell0.as_bytes());
        vt.extend_from_slice(cell1.as_bytes());
        let (buf, generations) = buf_and_generations(20, 5, &vt);

        let images = extract_frame_images(&buf, &generations);
        assert_eq!(images.len(), 1, "one virtual run");
        match images[0].kind {
            FrameImageKind::Virtual {
                run,
                placement_cols,
                screen_col,
                screen_line,
                ..
            } => {
                assert_eq!(run.image_id, 7);
                assert_eq!(run.width, 2, "two inherited-column cells form one run");
                assert_eq!(placement_cols, 2);
                assert_eq!((screen_line, screen_col), (0, 0));
            }
            _ => panic!("expected virtual"),
        }
    }

    #[test]
    fn unmatched_placeholder_is_skipped() {
        // Placeholder cells reference image id 9, but no image 9 was transmitted, so
        // there is no matching virtual placement and no cached image → skipped.
        let d0 = DIACRITICS[0];
        let cell = format!("\x1b[38;2;0;0;9m{}{}{}", '\u{10EEEE}', d0, d0);
        let (buf, generations) = buf_and_generations(20, 5, cell.as_bytes());
        assert!(
            extract_frame_images(&buf, &generations).is_empty(),
            "no matching placement/image → no descriptor, no marker"
        );
    }

    #[test]
    fn placeholder_codepoint_is_suppressed_from_text() {
        let d0 = DIACRITICS[0];
        let mut vt = Vec::new();
        vt.extend_from_slice(b"\x1b_Ga=T,U=1,f=32,s=1,v=1,i=7,p=3,c=1,r=1;/wAA/w==\x1b\\");
        vt.extend_from_slice(format!("\x1b[38;2;0;0;7m{}{}{}", '\u{10EEEE}', d0, d0).as_bytes());
        let (buf, generations) = buf_and_generations(20, 5, &vt);
        let frame = TerminalFrame::from_render_buffer_with_selection(&buf, None, &generations);
        // The placeholder glyph never reaches shaped text (no U+10EEEE), but the cell
        // still occupies its column (blank).
        assert!(
            !frame.lines()[0].text().as_ref().contains('\u{10EEEE}'),
            "placeholder codepoint suppressed"
        );
    }

    #[test]
    fn z_layer_buckets_by_protocol_thresholds() {
        // Pure classifier check across the three protocol layers.
        let (buf, generations) =
            buf_and_generations(20, 5, b"\x1b_Ga=T,f=32,s=1,v=1,i=1;/wAA/w==\x1b\\");
        let mut img = extract_frame_images(&buf, &generations).remove(0);
        img.z = i32::MIN;
        assert_eq!(img.z_layer(), ZLayer::BelowBackground);
        img.z = -1;
        assert_eq!(img.z_layer(), ZLayer::BelowText);
        img.z = 0;
        assert_eq!(img.z_layer(), ZLayer::AboveText);
    }
}

/// Full-pipeline performance profile (manual, release-only). Puts every stage of
/// a fast-scrollback frame on ONE scale so engine-side costs can be compared
/// against the real render-thread cost.
///
/// ```text
/// cargo test --release -p app full_frame_profile -- --ignored --nocapture
/// ```
///
/// Stages, in pipeline order:
///   1. parse    — `engine.write_vt` of 20k distinct 72-col lines (runs on the PTY
///                 thread today, off the frame critical path).
///   2. snapshot — `engine.snapshot` of the live viewport (once per rendered frame).
///   3. extract  — forced full extraction plus a one-row incremental update of the
///                 viewport (the live-region materialization, render thread).
///   4. shape    — real DirectWrite `layout_line` of NOVEL lines (render thread,
///                 cache-miss cost). Production caches shaped lines by hash, so
///                 repeated output is ~free; novel output pays this per line.
///
/// GPU submission is excluded (GPUI's own bench harness excludes it off-macOS).
#[cfg(test)]
mod full_frame_profile {
    use std::time::{Duration, Instant};
    use std::{fs, hint};

    use gpui::{FontRun, Platform, font, px};
    use gpui_windows::WindowsPlatform;
    use nmt_terminal::ghostty::GhosttyTerminal;
    use nmt_terminal::render_buffer::RenderBuffer;

    use super::{GenerationMap, TerminalFrame};

    const COLS: u16 = 80;
    const ROWS: u16 = 24;
    const CELLS_PER_LINE: usize = 72;
    const LINES: usize = 20_000;
    const FRAMES: usize = 1_000;

    /// Distinct 72-column content per line so shaping never hits a cache
    /// (worst case: novel program output, e.g. `cat` of a source tree).
    fn line_text(i: usize) -> String {
        let body =
            format!("{i:06} the quick brown fox jumps over the lazy dog {i:x} 0123456789abcdef");
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
        let mut previous =
            TerminalFrame::from_render_buffer_with_selection(&render_buf, None, &gens);
        let mut incremental_total = Duration::ZERO;
        for i in 0..FRAMES {
            engine.write_vt(if i % 2 == 0 { b"\rA" } else { b"\rB" });
            engine.snapshot_into(&mut render_buf).unwrap();
            let e = Instant::now();
            let frame = TerminalFrame::from_render_buffer_reusing(
                &render_buf,
                None,
                &gens,
                Some(&previous),
            );
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
        let per_frame_shape =
            Duration::from_secs_f64(shape.as_secs_f64() / LINES as f64 * ROWS as f64);
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
}
