use std::mem;

use nmt_config::colors::{AnsiColor, NamedColor};

use crate::grid::*;

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
fn wrapline_updates_preserve_other_cell_flags() {
    let mut s = Square(0);

    s.set_cell_flags(CellFlags::GRAPHEME);

    s.set_wrapline(true);

    assert!(s.wrapline());
    assert!(s.has_grapheme());
    assert!(!s.has_hyperlink());

    s.set_wrapline(false);

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

    s.set_wrapline(true);

    assert_eq!(s.c(), 'Z');
    assert_eq!(s.style_id(), 0x1234);
    assert_eq!(s.extras_id(), Some(0x5678));
    assert_eq!(s.wide(), Wide::Wide);
    assert!(s.wrapline());
}

#[test]
fn default_style_at_id_zero() {
    let set = StyleSet::new();

    assert_eq!(set.get(DEFAULT_STYLE_ID), Style::default());
    assert_eq!(set.len(), 1);
}

#[test]
fn intern_returns_existing_id() {
    let mut set = StyleSet::new();

    let s = Style {
        fg: AnsiColor::Named(NamedColor::Red),
        ..Style::default()
    };

    let id1 = set.intern(s);
    let id2 = set.intern(s);

    assert_eq!(id1, id2);
    assert_ne!(id1, DEFAULT_STYLE_ID);
    assert_eq!(set.len(), 2);
}

#[test]
fn distinct_styles_get_distinct_ids() {
    let mut set = StyleSet::new();

    let red = Style {
        fg: AnsiColor::Named(NamedColor::Red),
        ..Style::default()
    };

    let blue = Style {
        fg: AnsiColor::Named(NamedColor::Blue),
        ..Style::default()
    };

    assert_ne!(set.intern(red), set.intern(blue));
    assert_eq!(set.len(), 3);
}
