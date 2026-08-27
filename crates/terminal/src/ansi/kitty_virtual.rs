// Kitty graphics protocol virtual placement encoding/decoding

use nmt_config::colors::{AnsiColor, ColorRgb};

/// The Kitty Unicode placeholder codepoint (U+10EEEE). Cells containing
/// this codepoint are interpreted as image placeholders; their fg color
/// + grapheme combining marks encode which image and which slice.
pub const PLACEHOLDER: char = '\u{10EEEE}';

/// Diacritics used for row/column encoding in Kitty virtual placements
/// Index in array = the value being encoded
/// Derived from: https://sw.kovidgoyal.net/kitty/_downloads/f0a0de9ec8d9ff4456206db8e0814937/rowcolumn-diacritics.txt
pub const DIACRITICS: &[char] = &[
    '\u{0305}',
    '\u{030D}',
    '\u{030E}',
    '\u{0310}',
    '\u{0312}',
    '\u{033D}',
    '\u{033E}',
    '\u{033F}',
    '\u{0346}',
    '\u{034A}',
    '\u{034B}',
    '\u{034C}',
    '\u{0350}',
    '\u{0351}',
    '\u{0352}',
    '\u{0357}',
    '\u{035B}',
    '\u{0363}',
    '\u{0364}',
    '\u{0365}',
    '\u{0366}',
    '\u{0367}',
    '\u{0368}',
    '\u{0369}',
    '\u{036A}',
    '\u{036B}',
    '\u{036C}',
    '\u{036D}',
    '\u{036E}',
    '\u{036F}',
    '\u{0483}',
    '\u{0484}',
    '\u{0485}',
    '\u{0486}',
    '\u{0487}',
    '\u{0592}',
    '\u{0593}',
    '\u{0594}',
    '\u{0595}',
    '\u{0597}',
    '\u{0598}',
    '\u{0599}',
    '\u{059C}',
    '\u{059D}',
    '\u{059E}',
    '\u{059F}',
    '\u{05A0}',
    '\u{05A1}',
    '\u{05A8}',
    '\u{05A9}',
    '\u{05AB}',
    '\u{05AC}',
    '\u{05AF}',
    '\u{05C4}',
    '\u{0610}',
    '\u{0611}',
    '\u{0612}',
    '\u{0613}',
    '\u{0614}',
    '\u{0615}',
    '\u{0616}',
    '\u{0617}',
    '\u{0657}',
    '\u{0658}',
    '\u{0659}',
    '\u{065A}',
    '\u{065B}',
    '\u{065D}',
    '\u{065E}',
    '\u{06D6}',
    '\u{06D7}',
    '\u{06D8}',
    '\u{06D9}',
    '\u{06DA}',
    '\u{06DB}',
    '\u{06DC}',
    '\u{06DF}',
    '\u{06E0}',
    '\u{06E1}',
    '\u{06E2}',
    '\u{06E4}',
    '\u{06E7}',
    '\u{06E8}',
    '\u{06EB}',
    '\u{06EC}',
    '\u{0730}',
    '\u{0732}',
    '\u{0733}',
    '\u{0735}',
    '\u{0736}',
    '\u{073A}',
    '\u{073D}',
    '\u{073F}',
    '\u{0740}',
    '\u{0741}',
    '\u{0743}',
    '\u{0745}',
    '\u{0747}',
    '\u{0749}',
    '\u{074A}',
    '\u{07EB}',
    '\u{07EC}',
    '\u{07ED}',
    '\u{07EE}',
    '\u{07EF}',
    '\u{07F0}',
    '\u{07F1}',
    '\u{07F3}',
    '\u{0816}',
    '\u{0817}',
    '\u{0818}',
    '\u{0819}',
    '\u{081B}',
    '\u{081C}',
    '\u{081D}',
    '\u{081E}',
    '\u{081F}',
    '\u{0820}',
    '\u{0821}',
    '\u{0822}',
    '\u{0823}',
    '\u{0825}',
    '\u{0826}',
    '\u{0827}',
    '\u{0829}',
    '\u{082A}',
    '\u{082B}',
    '\u{082C}',
    '\u{082D}',
    '\u{0951}',
    '\u{0953}',
    '\u{0954}',
    '\u{0F82}',
    '\u{0F83}',
    '\u{0F86}',
    '\u{0F87}',
    '\u{135D}',
    '\u{135E}',
    '\u{135F}',
    '\u{17DD}',
    '\u{193A}',
    '\u{1A17}',
    '\u{1A75}',
    '\u{1A76}',
    '\u{1A77}',
    '\u{1A78}',
    '\u{1A79}',
    '\u{1A7A}',
    '\u{1A7B}',
    '\u{1A7C}',
    '\u{1B6B}',
    '\u{1B6D}',
    '\u{1B6E}',
    '\u{1B6F}',
    '\u{1B70}',
    '\u{1B71}',
    '\u{1B72}',
    '\u{1B73}',
    '\u{1CD0}',
    '\u{1CD1}',
    '\u{1CD2}',
    '\u{1CDA}',
    '\u{1CDB}',
    '\u{1CE0}',
    '\u{1DC0}',
    '\u{1DC1}',
    '\u{1DC3}',
    '\u{1DC4}',
    '\u{1DC5}',
    '\u{1DC6}',
    '\u{1DC7}',
    '\u{1DC8}',
    '\u{1DC9}',
    '\u{1DCB}',
    '\u{1DCC}',
    '\u{1DD1}',
    '\u{1DD2}',
    '\u{1DD3}',
    '\u{1DD4}',
    '\u{1DD5}',
    '\u{1DD6}',
    '\u{1DD7}',
    '\u{1DD8}',
    '\u{1DD9}',
    '\u{1DDA}',
    '\u{1DDB}',
    '\u{1DDC}',
    '\u{1DDD}',
    '\u{1DDE}',
    '\u{1DDF}',
    '\u{1DE0}',
    '\u{1DE1}',
    '\u{1DE2}',
    '\u{1DE3}',
    '\u{1DE4}',
    '\u{1DE5}',
    '\u{1DE6}',
    '\u{1DFE}',
    '\u{20D0}',
    '\u{20D1}',
    '\u{20D4}',
    '\u{20D5}',
    '\u{20D6}',
    '\u{20D7}',
    '\u{20DB}',
    '\u{20DC}',
    '\u{20E1}',
    '\u{20E7}',
    '\u{20E9}',
    '\u{20F0}',
    '\u{2CEF}',
    '\u{2CF0}',
    '\u{2CF1}',
    '\u{2DE0}',
    '\u{2DE1}',
    '\u{2DE2}',
    '\u{2DE3}',
    '\u{2DE4}',
    '\u{2DE5}',
    '\u{2DE6}',
    '\u{2DE7}',
    '\u{2DE8}',
    '\u{2DE9}',
    '\u{2DEA}',
    '\u{2DEB}',
    '\u{2DEC}',
    '\u{2DED}',
    '\u{2DEE}',
    '\u{2DEF}',
    '\u{2DF0}',
    '\u{2DF1}',
    '\u{2DF2}',
    '\u{2DF3}',
    '\u{2DF4}',
    '\u{2DF5}',
    '\u{2DF6}',
    '\u{2DF7}',
    '\u{2DF8}',
    '\u{2DF9}',
    '\u{2DFA}',
    '\u{2DFB}',
    '\u{2DFC}',
    '\u{2DFD}',
    '\u{2DFE}',
    '\u{2DFF}',
    '\u{A66F}',
    '\u{A67C}',
    '\u{A67D}',
    '\u{A6F0}',
    '\u{A6F1}',
    '\u{A8E0}',
    '\u{A8E1}',
    '\u{A8E2}',
    '\u{A8E3}',
    '\u{A8E4}',
    '\u{A8E5}',
    '\u{A8E6}',
    '\u{A8E7}',
    '\u{A8E8}',
    '\u{A8E9}',
    '\u{A8EA}',
    '\u{A8EB}',
    '\u{A8EC}',
    '\u{A8ED}',
    '\u{A8EE}',
    '\u{A8EF}',
    '\u{A8F0}',
    '\u{A8F1}',
    '\u{AAB0}',
    '\u{AAB2}',
    '\u{AAB3}',
    '\u{AAB7}',
    '\u{AAB8}',
    '\u{AABE}',
    '\u{AABF}',
    '\u{AAC1}',
    '\u{FE20}',
    '\u{FE21}',
    '\u{FE22}',
    '\u{FE23}',
    '\u{FE24}',
    '\u{FE25}',
    '\u{FE26}',
    '\u{10A0F}',
    '\u{10A38}',
    '\u{1D185}',
    '\u{1D186}',
    '\u{1D187}',
    '\u{1D188}',
    '\u{1D189}',
    '\u{1D1AA}',
    '\u{1D1AB}',
    '\u{1D1AC}',
    '\u{1D1AD}',
    '\u{1D242}',
    '\u{1D243}',
    '\u{1D244}',
];

/// Convert an index (0-based) to a diacritic character
/// Returns None if index is out of range
pub fn index_to_diacritic(index: u32) -> Option<char> {
    DIACRITICS.get(index as usize).copied()
}

/// Convert a diacritic character to an index (0-based)
/// Returns None if not a valid diacritic
pub fn diacritic_to_index(c: char) -> Option<u32> {
    DIACRITICS.iter().position(|&d| d == c).map(|i| i as u32)
}

/// Convert an image ID to RGB color (lower 24 bits)
pub fn id_to_rgb(id: u32) -> ColorRgb {
    ColorRgb {
        r: ((id >> 16) & 0xFF) as u8,
        g: ((id >> 8) & 0xFF) as u8,
        b: (id & 0xFF) as u8,
    }
}

/// Convert RGB color to image ID (24 bits)
pub fn rgb_to_id(rgb: ColorRgb) -> u32 {
    ((rgb.r as u32) << 16) | ((rgb.g as u32) << 8) | (rgb.b as u32)
}

/// Encode virtual placement data into a string with placeholder + diacritics
///
/// Kitty placeholder encoding:
/// - Base character: U+10EEEE (placeholder)
/// - 1st diacritic: Row index (0-based)
/// - 2nd diacritic: Column index (0-based)
/// - 3rd diacritic: High 8 bits of image_id (optional, for IDs > 16M)
///
/// Image ID (lower 24 bits) is encoded in foreground color
/// Placement ID is encoded in underline color
pub fn encode_placeholder(row: u32, col: u32, image_id_high: Option<u8>) -> String {
    let mut result = String::from('\u{10EEEE}');

    // Add row diacritic
    if let Some(d) = index_to_diacritic(row) {
        result.push(d);
    }

    // Add column diacritic
    if let Some(d) = index_to_diacritic(col) {
        result.push(d);
    }

    // Add high byte diacritic if needed
    if let Some(high) = image_id_high
        && let Some(d) = index_to_diacritic(high as u32)
    {
        result.push(d);
    }

    result
}

/// Decode placement information from a placeholder character string with diacritics
///
/// Returns (row_index, col_index, image_id_high) if successfully decoded
/// The base character should be U+10EEEE
pub fn decode_placeholder(s: &str) -> Option<(u32, u32, Option<u8>)> {
    let mut chars = s.chars();

    // First character should be the placeholder
    if chars.next()? != '\u{10EEEE}' {
        return None;
    }

    // 1st diacritic: row index
    let row = chars.next().and_then(diacritic_to_index)?;

    // 2nd diacritic: column index
    let col = chars.next().and_then(diacritic_to_index)?;

    // 3rd diacritic (optional): high 8 bits of image_id
    let high = chars.next().and_then(|c| {
        diacritic_to_index(c).and_then(|idx| if idx <= 255 { Some(idx as u8) } else { None })
    });

    Some((row, col, high))
}

/// Map an `AnsiColor` to a 24-bit id used by the placeholder protocol.
///
/// The foreground and underline colors encode the lower 24 bits of the image
/// or placement id. The encoding depends on color mode:
/// - `Indexed(n)`: id = n (0..=255). 256-color slot maps to itself.
/// - `Spec(rgb)`: id = (R << 16) | (G << 8) | B (full 24 bits).
/// - `Named(_)`: id = 0 (no encoding possible — caller treats as "no id").
fn color_to_id(color: AnsiColor) -> u32 {
    match color {
        AnsiColor::Indexed(n) => n as u32,
        AnsiColor::Spec(rgb) => rgb_to_id(rgb),
        AnsiColor::Named(_) => 0,
    }
}

/// Per-cell decode of a U+10EEEE placeholder, before continuation rules
/// resolve missing diacritics. Mirrors ghostty's `IncompletePlacement`
///: row / col / `image_id_high` are
/// `Option` because kitty allows applications to omit later diacritics
/// when they would inherit from the previous cell.
///
/// `image_id_low` and `placement_id` always have a value (0 if the
/// foreground / underline color can't carry an id).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IncompletePlacement {
    pub image_id_low: u32,
    pub image_id_high: Option<u8>,
    pub placement_id: u32,
    pub row: Option<u32>,
    pub col: Option<u32>,
    /// Run width in cells. `from_cell` returns 1; `append` increments.
    pub width: u32,
}

impl IncompletePlacement {
    /// Decode a single cell. The combining marks are the cell's grapheme
    /// zerowidth chars (the grid's `Extras.zerowidth`); kitty puts up to 3:
    /// `[row, col, image_id_high]`. All three are optional; invalid
    /// diacritics are silently dropped (matches ghostty's "treat as if
    /// they don't exist" comment in `graphics_unicode.zig:453-455`).
    pub fn from_cell(fg: AnsiColor, underline: Option<AnsiColor>, combining: &[char]) -> Self {
        let row = combining.first().copied().and_then(diacritic_to_index);
        let col = combining.get(1).copied().and_then(diacritic_to_index);
        let image_id_high = combining
            .get(2)
            .copied()
            .and_then(diacritic_to_index)
            .and_then(|i| if i <= 255 { Some(i as u8) } else { None });
        Self {
            image_id_low: color_to_id(fg),
            image_id_high,
            placement_id: underline.map(color_to_id).unwrap_or(0),
            row,
            col,
            width: 1,
        }
    }

    /// True if `other` (the next cell in the same row) can extend this
    /// run. Mirrors ghostty's `canAppend`:
    /// - same `image_id_low` and `placement_id`
    /// - `other.row` is missing (inherit) or matches `self.row`
    /// - `other.col` is missing (inherit + auto-increment) or equals
    ///   `self.col + self.width` (sequential)
    /// - `other.image_id_high` is missing or matches
    pub fn can_append(&self, other: &IncompletePlacement) -> bool {
        self.image_id_low == other.image_id_low
            && self.placement_id == other.placement_id
            && (other.row.is_none() || other.row == self.row)
            && match other.col {
                None => true,
                Some(c) => self.col.map(|sc| c == sc + self.width).unwrap_or(false),
            }
            && (other.image_id_high.is_none() || other.image_id_high == self.image_id_high)
    }

    /// Extend the run by one cell. Caller must check `can_append` first.
    pub fn append(&mut self) {
        self.width += 1;
    }

    /// Resolve the run into a final placement, defaulting any still-`None`
    /// fields. Mirrors ghostty's `complete()`.
    pub fn complete(&self) -> PlaceholderRun {
        PlaceholderRun {
            image_id: ((self.image_id_high.unwrap_or(0) as u32) << 24)
                | (self.image_id_low & 0x00FF_FFFF),
            placement_id: self.placement_id,
            row: self.row.unwrap_or(0),
            col: self.col.unwrap_or(0),
            width: self.width,
        }
    }
}

/// One row of cells of a virtual placement that all show consecutive
/// columns of the same image row. Returned by `IncompletePlacement::complete`.
/// The renderer produces ONE `GraphicOverlay` per `PlaceholderRun`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaceholderRun {
    pub image_id: u32,
    pub placement_id: u32,
    /// Image row (0-indexed) — within the placement's `rows` grid.
    pub row: u32,
    /// Leftmost image column (0-indexed) — within the placement's `cols` grid.
    pub col: u32,
    /// Number of cells in this run (each cell = one column).
    pub width: u32,
}

/// Output of `compute_run_geometry` — what the renderer should actually
/// draw for a `PlaceholderRun`. All values in pixels, in the screen's
/// coordinate space (offsets already applied).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RunGeometry {
    /// Top-left of the rendered image slice.
    pub x: f32,
    pub y: f32,
    /// Pixel size of the rendered slice.
    pub width: f32,
    pub height: f32,
    /// Source rect on the image, normalised `[u0, v0, u1, v1]`.
    pub source_rect: [f32; 4],
}

/// Compute the screen rect + source rect for one row-run, taking the
/// placement's grid size, the image's pixel size, and the cell metrics.
/// Mirrors ghostty's `Placement.renderPlacement`
///, specialised for a single row of
/// cells (height = 1 cell). Returns `None` if the run lies entirely in
/// the centering padding (no pixels to draw).
///
/// Algorithm:
/// 1. Compute placement-box pixel size: `cols × rows × cell`.
/// 2. Aspect-fit the image inside the box → `fit_w × fit_h` plus
///    centering padding `pad_x` / `pad_y`.
/// 3. Compute the run's rect in placement-box coordinates.
/// 4. Intersect with the image's fitted rect — anything in the
///    centering padding is dropped.
/// 5. Map the visible intersection back to a normalised source rect on
///    the image, and to a screen position (the run's leftmost cell +
///    intra-cell offset for clipping).
///
/// `origin_x`/`origin_y` are the top-left of the terminal grid in
/// physical pixels. `screen_line` is the visible-rows index of the run.
/// `start_screen_col` is the run's leftmost cell on screen.
#[allow(clippy::too_many_arguments)]
pub fn compute_run_geometry(
    run: &PlaceholderRun,
    placement_cols: u32,
    placement_rows: u32,
    image_width_px: u32,
    image_height_px: u32,
    cell_width: f32,
    cell_height: f32,
    origin_x: f32,
    origin_y: f32,
    screen_line: usize,
    start_screen_col: usize,
) -> Option<RunGeometry> {
    let img_w = image_width_px as f32;
    let img_h = image_height_px as f32;
    if img_w <= 0.0 || img_h <= 0.0 {
        return None;
    }

    // 1. Placement box in pixels.
    let p_cols_px = placement_cols.max(1) as f32 * cell_width;
    let p_rows_px = placement_rows.max(1) as f32 * cell_height;

    // 2. Aspect-fit + centering.
    let scale = (p_cols_px / img_w).min(p_rows_px / img_h);
    let fit_w = img_w * scale;
    let fit_h = img_h * scale;
    let pad_x = (p_cols_px - fit_w) * 0.5;
    let pad_y = (p_rows_px - fit_h) * 0.5;

    // 3. Run rect inside the placement box.
    let run_box_x = run.col as f32 * cell_width;
    let run_box_y = run.row as f32 * cell_height;
    let run_box_w = run.width as f32 * cell_width;
    let run_box_h = cell_height;

    // 4. Intersect with image fitted rect.
    let img_box_x0 = pad_x;
    let img_box_y0 = pad_y;
    let img_box_x1 = pad_x + fit_w;
    let img_box_y1 = pad_y + fit_h;

    let vis_x0 = run_box_x.max(img_box_x0);
    let vis_y0 = run_box_y.max(img_box_y0);
    let vis_x1 = (run_box_x + run_box_w).min(img_box_x1);
    let vis_y1 = (run_box_y + run_box_h).min(img_box_y1);
    if vis_x1 <= vis_x0 || vis_y1 <= vis_y0 {
        return None;
    }

    // 5. Source rect (normalised) and screen position.
    let src_u0 = (vis_x0 - img_box_x0) / fit_w;
    let src_v0 = (vis_y0 - img_box_y0) / fit_h;
    let src_u1 = (vis_x1 - img_box_x0) / fit_w;
    let src_v1 = (vis_y1 - img_box_y0) / fit_h;

    let intra_x = vis_x0 - run_box_x;
    let intra_y = vis_y0 - run_box_y;
    Some(RunGeometry {
        x: origin_x + start_screen_col as f32 * cell_width + intra_x,
        y: origin_y + screen_line as f32 * cell_height + intra_y,
        width: vis_x1 - vis_x0,
        height: vis_y1 - vis_y0,
        source_rect: [src_u0, src_v0, src_u1, src_v1],
    })
}
