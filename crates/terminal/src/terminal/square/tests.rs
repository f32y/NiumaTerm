use std::mem;

use crate::terminal::square::*;

#[test]
fn square_is_eight_bytes() {
    // The whole point of this rewrite.
    assert_eq!(mem::size_of::<Square>(), 8);
}

#[test]
fn codepoint_round_trip() {
    let mut s = Square(0);
    s.set_c('🦀');
    assert_eq!(s.c(), '🦀');
    s.set_c('a');
    assert_eq!(s.c(), 'a');
    s.set_c('\0');
    assert_eq!(s.c(), '\0');
}

#[test]
fn style_id_round_trip() {
    let mut s = Square(0);
    s.set_style_id(42);
    assert_eq!(s.style_id(), 42);
    s.set_style_id(0xFFFF);
    assert_eq!(s.style_id(), 0xFFFF);
}

#[test]
fn extras_id_round_trip() {
    let mut s = Square(0);
    assert_eq!(s.extras_id(), None);
    s.set_extras_id(Some(7));
    assert_eq!(s.extras_id(), Some(7));
    s.set_extras_id(None);
    assert_eq!(s.extras_id(), None);
}

#[test]
fn wide_round_trip() {
    let mut s = Square(0);
    for w in [Wide::Narrow, Wide::Wide, Wide::Spacer, Wide::LeadingSpacer] {
        s.set_wide(w);
        assert_eq!(s.wide(), w);
    }
}

#[test]
fn cell_flags_round_trip() {
    let mut s = Square(0);
    s.insert_cell_flag(CellFlags::WRAPLINE | CellFlags::GRAPHEME);
    assert!(s.wrapline());
    assert!(s.has_grapheme());
    assert!(!s.has_hyperlink());
    s.remove_cell_flag(CellFlags::WRAPLINE);
    assert!(!s.wrapline());
    assert!(s.has_grapheme());
}

#[test]
fn fields_are_independent() {
    let mut s = Square(0);
    s.set_c('Z');
    s.set_style_id(0x1234);
    s.set_extras_id(Some(0x5678));
    s.set_wide(Wide::Wide);
    s.insert_cell_flag(CellFlags::WRAPLINE);
    assert_eq!(s.c(), 'Z');
    assert_eq!(s.style_id(), 0x1234);
    assert_eq!(s.extras_id(), Some(0x5678));
    assert_eq!(s.wide(), Wide::Wide);
    assert!(s.wrapline());
}

#[test]
fn bg_palette_round_trip() {
    let mut s = Square(0);
    s.set_bg_palette(42);
    assert_eq!(s.content_tag(), ContentTag::BgPalette);
    assert!(s.is_bg_only());
    assert_eq!(s.bg_palette_index(), 42);
    // bg-only cells have no codepoint
    assert_eq!(s.c(), '\0');
    // NOTE: `style_id()` and `extras_id()` are intentionally
    // branchless on the bg-only/codepoint discriminant — they
    // read raw bits regardless of content_tag. For a bg-only
    // cell those bits hold the bg color encoding, not a style/
    // extras id. Production code (the renderer hot loop) checks
    // `content_tag()` first, so it never asks `style_id()` of a
    // bg-only cell. We don't assert `style_id() == DEFAULT_STYLE_ID`
    // here for that reason. (See comments on `Square::style_id`.)
}

#[test]
fn bg_rgb_round_trip() {
    let mut s = Square(0);
    s.set_bg_rgb(0x12, 0x34, 0x56);
    assert_eq!(s.content_tag(), ContentTag::BgRgb);
    assert!(s.is_bg_only());
    assert_eq!(s.bg_rgb(), (0x12, 0x34, 0x56));
    assert_eq!(s.c(), '\0');
    // Same caveat as `bg_palette_round_trip`: branchless
    // accessors read raw bits, so `style_id()` of a BgRgb cell
    // returns garbage. The renderer always checks `content_tag()`
    // first.
}

#[test]
fn bg_only_preserves_wrapline() {
    let mut s = Square(0);
    s.set_wrapline(true);
    s.set_bg_rgb(1, 2, 3);
    assert!(s.wrapline());
    assert_eq!(s.bg_rgb(), (1, 2, 3));
}
