use nmt_config::colors::{AnsiColor, NamedColor};
use nmt_terminal::terminal::style::*;

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
