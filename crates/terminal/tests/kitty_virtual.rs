use nmt_config::colors::{AnsiColor, ColorRgb};
use nmt_terminal::ansi::kitty_virtual::*;

#[test]
fn test_diacritic_conversion() {
    // First diacritic should encode 0
    assert_eq!(index_to_diacritic(0), Some('\u{0305}'));
    assert_eq!(diacritic_to_index('\u{0305}'), Some(0));

    // Last diacritic
    let last_idx = (DIACRITICS.len() - 1) as u32;
    assert_eq!(index_to_diacritic(last_idx), Some('\u{1D244}'));
    assert_eq!(diacritic_to_index('\u{1D244}'), Some(last_idx));
}

#[test]
fn test_rgb_id_conversion() {
    let rgb = ColorRgb {
        r: 0x12,
        g: 0x34,
        b: 0x56,
    };
    let id = rgb_to_id(rgb);
    assert_eq!(id, 0x123456);
    assert_eq!(id_to_rgb(id), rgb);
}

#[test]
fn test_encode_placeholder() {
    // Encode row=0, col=0
    let s = encode_placeholder(0, 0, None);
    assert!(s.starts_with('\u{10EEEE}'));
    assert_eq!(s.chars().count(), 3); // placeholder + 2 diacritics

    // With high byte
    let s = encode_placeholder(0, 0, Some(1));
    assert_eq!(s.chars().count(), 4); // placeholder + 3 diacritics
}

#[test]
fn test_encode_decode_roundtrip() {
    // Test encoding and decoding
    let encoded = encode_placeholder(5, 10, None);
    let decoded = decode_placeholder(&encoded).unwrap();
    assert_eq!(decoded, (5, 10, None));

    // With high byte
    let encoded = encode_placeholder(5, 10, Some(42));
    let decoded = decode_placeholder(&encoded).unwrap();
    assert_eq!(decoded, (5, 10, Some(42)));
}

use nmt_config::colors::NamedColor;

#[test]
fn from_cell_indexed_fg_two_diacritics() {
    // kitten icat with palette IDs ≤ 255: image_id_low = palette
    // index, no high byte, no placement_id.
    let combining = [DIACRITICS[3], DIACRITICS[7]]; // row=3, col=7
    let p = IncompletePlacement::from_cell(AnsiColor::Indexed(42), None, &combining);
    assert_eq!(p.image_id_low, 42);
    assert_eq!(p.image_id_high, None);
    assert_eq!(p.placement_id, 0);
    assert_eq!(p.row, Some(3));
    assert_eq!(p.col, Some(7));
    let run = p.complete();
    assert_eq!(run.image_id, 42);
    assert_eq!(run.row, 3);
    assert_eq!(run.col, 7);
    assert_eq!(run.width, 1);
}

#[test]
fn from_cell_rgb_fg_three_diacritics() {
    // Default kitten icat --unicode-placeholder: 32-bit id, true-color
    // fg encodes lower 24 bits, 3rd diacritic encodes upper 8 bits.
    // Reproduces kitty's `kittens/icat/transmit.go:236-244`.
    let rgb = ColorRgb {
        r: 0xAB,
        g: 0xCD,
        b: 0xEF,
    };
    let combining = [DIACRITICS[0], DIACRITICS[1], DIACRITICS[2]];
    // 1st = row=0, 2nd = col=1, 3rd = high=2
    let p = IncompletePlacement::from_cell(AnsiColor::Spec(rgb), None, &combining);
    assert_eq!(p.image_id_low, 0x00AB_CDEF);
    assert_eq!(p.image_id_high, Some(2));
    assert_eq!(p.row, Some(0));
    assert_eq!(p.col, Some(1));
    let run = p.complete();
    assert_eq!(run.image_id, 0x0200_0000 | 0x00AB_CDEF);
}

#[test]
fn from_cell_with_placement_id_underline() {
    let fg_rgb = ColorRgb { r: 1, g: 2, b: 3 };
    let ul_rgb = ColorRgb { r: 0, g: 0, b: 99 };
    let combining = [DIACRITICS[0], DIACRITICS[0]];
    let p = IncompletePlacement::from_cell(
        AnsiColor::Spec(fg_rgb),
        Some(AnsiColor::Spec(ul_rgb)),
        &combining,
    );
    assert_eq!(p.image_id_low, 0x0001_0203);
    assert_eq!(p.placement_id, 99);
}

#[test]
fn from_cell_missing_diacritics_yields_none_fields() {
    // Continuation rules: missing diacritics produce `None` for those
    // fields, so the caller can inherit from the previous cell.
    let p = IncompletePlacement::from_cell(AnsiColor::Indexed(1), None, &[]);
    assert_eq!(p.row, None);
    assert_eq!(p.col, None);
    assert_eq!(p.image_id_high, None);
    assert_eq!(p.image_id_low, 1);

    let p = IncompletePlacement::from_cell(AnsiColor::Indexed(1), None, &[DIACRITICS[5]]);
    assert_eq!(p.row, Some(5));
    assert_eq!(p.col, None);

    // `complete()` defaults missing fields to 0.
    let run = p.complete();
    assert_eq!(run.row, 5);
    assert_eq!(run.col, 0);
}

#[test]
fn from_cell_named_fg_yields_zero_id() {
    let combining = [DIACRITICS[0], DIACRITICS[0]];
    let p =
        IncompletePlacement::from_cell(AnsiColor::Named(NamedColor::Foreground), None, &combining);
    assert_eq!(p.image_id_low, 0);
}

fn p(row: Option<u32>, col: Option<u32>) -> IncompletePlacement {
    IncompletePlacement {
        image_id_low: 7,
        image_id_high: None,
        placement_id: 0,
        row,
        col,
        width: 1,
    }
}

#[test]
fn can_append_inherits_row_and_col() {
    // Empty cell (no diacritics) right after a fully-decoded cell:
    // inherit row, auto-increment col.
    let mut a = p(Some(0), Some(0));
    let b = p(None, None);
    assert!(a.can_append(&b));
    a.append();
    assert_eq!(a.width, 2);
}

#[test]
fn can_append_explicit_sequential_col() {
    let a = p(Some(0), Some(0));
    let b = p(Some(0), Some(1));
    assert!(a.can_append(&b));
}

#[test]
fn can_append_inherit_row_explicit_col() {
    let a = p(Some(0), Some(0));
    let b = p(None, Some(1));
    assert!(a.can_append(&b));
}

#[test]
fn cannot_append_col_jump() {
    // Skipping a column breaks the run.
    let a = p(Some(0), Some(0));
    let b = p(Some(0), Some(2));
    assert!(!a.can_append(&b));
}

#[test]
fn cannot_append_different_row() {
    let a = p(Some(0), Some(0));
    let b = p(Some(1), Some(1));
    assert!(!a.can_append(&b));
}

#[test]
fn cannot_append_different_image_id() {
    let mut a = p(Some(0), Some(0));
    a.image_id_low = 1;
    let mut b = p(Some(0), Some(1));
    b.image_id_low = 2;
    assert!(!a.can_append(&b));
}

#[test]
fn cannot_append_different_image_id_high() {
    let mut a = p(Some(0), Some(0));
    a.image_id_high = Some(5);
    let mut b = p(Some(0), Some(1));
    b.image_id_high = Some(6);
    assert!(!a.can_append(&b));
}

#[test]
fn can_append_inherits_image_id_high() {
    let mut a = p(Some(0), Some(0));
    a.image_id_high = Some(5);
    let b = p(Some(0), Some(1)); // image_id_high = None
    assert!(a.can_append(&b));
}

fn approx(a: f32, b: f32) {
    assert!((a - b).abs() < 1e-4, "expected ~{b}, got {a}");
}

fn run(row: u32, col: u32, width: u32) -> PlaceholderRun {
    PlaceholderRun {
        image_id: 1,
        placement_id: 0,
        row,
        col,
        width,
    }
}

#[test]
fn geom_image_matches_grid_aspect_no_padding() {
    // Image 100×50, placement 10×5, cell 10×10 → exact fit, no padding.
    // First cell-run (row=0, col=0..=2) covers the leftmost 30 px on
    // screen and the leftmost 30% of the image horizontally, top
    // 20% vertically (1 row out of 5).
    let g = compute_run_geometry(&run(0, 0, 3), 10, 5, 100, 50, 10.0, 10.0, 0.0, 0.0, 0, 0)
        .expect("visible");
    approx(g.x, 0.0);
    approx(g.y, 0.0);
    approx(g.width, 30.0);
    approx(g.height, 10.0);
    approx(g.source_rect[0], 0.0);
    approx(g.source_rect[1], 0.0);
    approx(g.source_rect[2], 0.30);
    approx(g.source_rect[3], 0.20);
}

#[test]
fn geom_image_taller_than_grid_centers_horizontally() {
    // Image 50×100, placement 10×10, cell 10×10. Placement box
    // 100×100, image fits height (scale 1.0), wastes 50 px width
    // (25 px padding each side). For a fully-visible placement
    // starting at screen (0, 0), the image col matches screen col.

    // Cells 0..=1 (image col=0..=1) → screen x 0..20, entirely in
    // the LEFT padding → returns None.
    let none = compute_run_geometry(&run(0, 0, 2), 10, 10, 50, 100, 10.0, 10.0, 0.0, 0.0, 0, 0);
    assert!(none.is_none(), "left-padding run should be culled");

    // Cell at image col=3 (placement box x=30..40) is inside the
    // image area (padding ends at x=25). For a fully-visible
    // placement screen_col matches image col → start_screen_col=3.
    let g = compute_run_geometry(&run(0, 3, 1), 10, 10, 50, 100, 10.0, 10.0, 0.0, 0.0, 0, 3)
        .expect("visible");
    // Visible intersection (in placement-box coords): 30..40 × 0..10.
    // intra_x = 30 - 30 = 0, so screen_x = 3*10 = 30.
    // Source x = (30 - 25)..(40 - 25) of fit_w=50 → u 0.10..0.30.
    // Source y = 0..10 of fit_h=100 → v 0..0.10.
    approx(g.x, 30.0);
    approx(g.y, 0.0);
    approx(g.width, 10.0);
    approx(g.height, 10.0);
    approx(g.source_rect[0], 0.10);
    approx(g.source_rect[1], 0.0);
    approx(g.source_rect[2], 0.30);
    approx(g.source_rect[3], 0.10);
}

#[test]
fn geom_image_wider_than_grid_centers_vertically() {
    // Image 200×50, placement 10×10, cell 10×10. Placement box
    // 100×100, fit width (scale 0.5), fit_h = 25, padding y =
    // (100-25)/2 = 37.5. Top + bottom rows entirely in padding.
    // For fully-visible placement at screen (0,0), image row =
    // screen line.

    // Row 0 (y 0..10): in top padding → None.
    let none = compute_run_geometry(&run(0, 0, 10), 10, 10, 200, 50, 10.0, 10.0, 0.0, 0.0, 0, 0);
    assert!(none.is_none());

    // Row 4 (y 40..50): inside image area (37.5..62.5).
    let g = compute_run_geometry(&run(4, 0, 10), 10, 10, 200, 50, 10.0, 10.0, 0.0, 0.0, 4, 0)
        .expect("visible");
    // Visible rect: y 40..50, x 0..100. intra_y = 40 - 40 = 0,
    // screen_y = 4*10 = 40. Image y 37.5..62.5 → src y 2.5..12.5
    // of fit_h=25 → v 0.10..0.50. Full width: u 0..1.
    approx(g.x, 0.0);
    approx(g.y, 40.0);
    approx(g.width, 100.0);
    approx(g.height, 10.0);
    approx(g.source_rect[0], 0.0);
    approx(g.source_rect[1], 0.10);
    approx(g.source_rect[2], 1.0);
    approx(g.source_rect[3], 0.50);
}

#[test]
fn geom_partial_visibility_scrolled_off_top() {
    // Placement scrolled half off-screen at the top. The run for
    // image row=2 is the FIRST visible row (rows 0 and 1 are
    // off-screen), so screen_line = 0 even though the run reports
    // image row = 2. Tests the partial-visibility clipping.
    //
    // Image 100×100, placement 10×10, cell 10×10 → exact fit, no
    // padding. Run at image row=2, col=0..=9 (full width).
    let g = compute_run_geometry(
        &run(2, 0, 10),
        10,
        10,
        100,
        100,
        10.0,
        10.0,
        0.0,
        0.0,
        0, // screen_line: top of viewport
        0, // start_screen_col: leftmost
    )
    .expect("visible");
    approx(g.x, 0.0);
    approx(g.y, 0.0); // rendered at top of viewport, not at row*cell
    approx(g.width, 100.0);
    approx(g.height, 10.0);
    // Source rect still picks the row-2 slice of the image.
    approx(g.source_rect[1], 0.20);
    approx(g.source_rect[3], 0.30);
}

#[test]
fn geom_origin_offset_applies_to_screen_pos_only() {
    // Same image as the no-padding case, but origin shifted to
    // (100, 50). Source rect must be unchanged; screen rect shifts.
    let g = compute_run_geometry(&run(0, 0, 3), 10, 5, 100, 50, 10.0, 10.0, 100.0, 50.0, 0, 0)
        .expect("visible");
    approx(g.x, 100.0);
    approx(g.y, 50.0);
    approx(g.source_rect[0], 0.0);
    approx(g.source_rect[2], 0.30);
}

#[test]
fn geom_screen_line_and_start_col_offset_screen_pos() {
    // Run reported at row=0 inside the placement, but rendered on
    // screen line 7, starting screen col 5. Screen y must be 7*cell.
    let g = compute_run_geometry(&run(0, 0, 2), 10, 5, 100, 50, 10.0, 10.0, 0.0, 0.0, 7, 5)
        .expect("visible");
    approx(g.x, 50.0);
    approx(g.y, 70.0);
}

#[test]
fn geom_returns_none_when_image_zero_sized() {
    let none = compute_run_geometry(&run(0, 0, 1), 10, 5, 0, 50, 10.0, 10.0, 0.0, 0.0, 0, 0);
    assert!(none.is_none());
}

#[test]
fn run_of_three_cells_with_only_first_diacritics() {
    // Common pattern: app emits diacritics on cell 0 only, leaving
    // cells 1 and 2 to inherit.
    let mut run = IncompletePlacement::from_cell(
        AnsiColor::Indexed(7),
        None,
        &[DIACRITICS[0], DIACRITICS[0]], // row=0, col=0
    );
    for _ in 0..2 {
        let next = IncompletePlacement::from_cell(AnsiColor::Indexed(7), None, &[]);
        assert!(run.can_append(&next));
        run.append();
    }
    let r = run.complete();
    assert_eq!(r.row, 0);
    assert_eq!(r.col, 0);
    assert_eq!(r.width, 3);
}
