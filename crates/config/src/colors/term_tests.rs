use crate::colors::term::List;
use crate::colors::{Colors, NamedColor};

#[test]
fn terminal_palette_includes_configured_cursor_color() {
    let colors = Colors {
        cursor: [0.25, 0.5, 0.75, 1.0],
        ..Colors::default()
    };

    let palette: List = (&colors).into();

    assert_eq!(palette[NamedColor::Cursor], colors.cursor);
}
