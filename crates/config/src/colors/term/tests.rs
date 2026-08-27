use crate::colors::term::List;
use crate::colors::{Colors, NamedColor};

#[test]
fn terminal_palette_includes_configured_cursor_color() {
    let colors = Colors::default();
    let palette = List::from(&colors);

    assert_eq!(palette[NamedColor::Cursor], colors.cursor);
}
