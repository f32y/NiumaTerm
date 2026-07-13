use crate::colors::{ColorArray, ColorComposition};

/// Parse a `"#RRGGBB"` literal into an `[r, g, b, 1.0]` sRGB array at compile
/// time. Defaults are constant, so this avoids the per-call regex compile and
/// String allocation that `ColorBuilder::from_hex` does at runtime.
const fn hex(s: &str) -> ColorArray {
    let b = s.as_bytes();
    let r = (nibble(b[1]) << 4 | nibble(b[2])) as f32 / 255.0;
    let g = (nibble(b[3]) << 4 | nibble(b[4])) as f32 / 255.0;
    let bl = (nibble(b[5]) << 4 | nibble(b[6])) as f32 / 255.0;
    [r, g, bl, 1.0]
}

const fn nibble(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        b'A'..=b'F' => c - b'A' + 10,
        _ => 0,
    }
}

#[inline]
pub fn background() -> ColorComposition {
    let color = hex("#0F0D0E");
    (
        color,
        crate::render_types::Color {
            r: color[0] as f64,
            g: color[1] as f64,
            b: color[2] as f64,
            a: color[3] as f64,
        },
    )
}

#[inline]
pub fn cursor() -> ColorArray {
    hex("#F712FF")
}

#[inline]
pub fn vi_cursor() -> ColorArray {
    hex("#12d0ff")
}

#[inline]
pub fn tabs() -> ColorArray {
    hex("#424040")
}

#[inline]
pub fn bar() -> ColorArray {
    hex("#1b1a1a")
}

#[inline]
pub fn tabs_active() -> ColorArray {
    [1., 1., 1., 1.]
}

#[inline]
pub fn tab_border() -> ColorArray {
    hex("#696767")
}

#[inline]
pub fn foreground() -> ColorArray {
    [1., 1., 1., 1.]
}

#[inline]
pub fn green() -> ColorArray {
    hex("#2AD947")
}

#[inline]
pub fn red() -> ColorArray {
    hex("#FF1261")
}

#[inline]
pub fn blue() -> ColorArray {
    hex("#2D9AFF")
}

#[inline]
pub fn yellow() -> ColorArray {
    hex("#FCBA28")
}

#[inline]
pub fn black() -> ColorArray {
    hex("#393A3D")
}

#[inline]
pub fn cyan() -> ColorArray {
    hex("#17d5df")
}

#[inline]
pub fn magenta() -> ColorArray {
    hex("#DD30FF")
}

#[inline]
pub fn white() -> ColorArray {
    hex("#E7E7E7")
}

#[inline]
pub fn default_light_black() -> ColorArray {
    hex("#6B6B6B")
}
#[inline]
pub fn default_light_blue() -> ColorArray {
    hex("#82B8C8")
}
#[inline]
pub fn default_light_cyan() -> ColorArray {
    hex("#93D3C3")
}
#[inline]
pub fn default_light_green() -> ColorArray {
    hex("#AAC474")
}
#[inline]
pub fn default_light_magenta() -> ColorArray {
    hex("#C28CB8")
}
#[inline]
pub fn default_light_red() -> ColorArray {
    hex("#C55555")
}
#[inline]
pub fn default_light_white() -> ColorArray {
    hex("#F8F8F8")
}
#[inline]
pub fn default_light_yellow() -> ColorArray {
    hex("#FECA88")
}

#[inline]
pub fn split() -> ColorArray {
    hex("#292527")
}

#[inline]
pub fn split_active() -> ColorArray {
    hex("#44C9F0")
}

#[inline]
pub fn selection_foreground() -> ColorArray {
    hex("#0F0D0E")
}

#[inline]
pub fn selection_background() -> ColorArray {
    hex("#C8C8C8")
}

#[inline]
pub fn search_match_background() -> ColorArray {
    hex("#44C9F0")
}
#[inline]
pub fn search_match_foreground() -> ColorArray {
    [1., 1., 1., 1.]
}
#[inline]
pub fn search_focused_match_background() -> ColorArray {
    hex("#E6A003")
}
#[inline]
pub fn search_focused_match_foreground() -> ColorArray {
    [1., 1., 1., 1.]
}
#[inline]
pub fn hint_foreground() -> ColorArray {
    // Dark text color (#181818)
    hex("#181818")
}
#[inline]
pub fn hint_background() -> ColorArray {
    // Orange background color (#f4bf75)
    hex("#f4bf75")
}
