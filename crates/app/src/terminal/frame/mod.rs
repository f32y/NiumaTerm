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
mod tests;

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
mod full_frame_profile;
