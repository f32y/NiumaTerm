pub mod defaults;
pub mod term;

use std::fmt;
use std::ops::Mul;

use serde::de::Error as DeError;
use serde::{Deserialize, Serialize, de};
use tracing::trace;

use crate::colors::defaults::*;

pub type ColorArray = [f32; 4];

#[derive(Debug, Default, Copy, Clone, Eq, PartialEq, Hash)]
pub struct ColorRgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Mul<f32> for ColorRgb {
    type Output = ColorRgb;

    fn mul(self, rhs: f32) -> ColorRgb {
        let r: f32 = self.r.into();
        let g: f32 = self.g.into();
        let b: f32 = self.b.into();

        let result = ColorRgb {
            r: (r * rhs).clamp(0.0, 255.0) as u8,
            g: (g * rhs).clamp(0.0, 255.0) as u8,
            b: (b * rhs).clamp(0.0, 255.0) as u8,
        };

        trace!(
            "Scaling ColorRgb by {} from {:?} to {:?}",
            rhs, self, result
        );

        result
    }
}

impl From<&ColorRgb> for ColorArray {
    fn from(color: &ColorRgb) -> ColorArray {
        Rgba::from_rgb(*color).into()
    }
}

impl From<(u8, u8, u8)> for ColorRgb {
    fn from((r, g, b): (u8, u8, u8)) -> Self {
        Self { r, g, b }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AnsiColor {
    Named(NamedColor),
    Spec(ColorRgb),
    Indexed(u8),
}

#[derive(Debug, Copy, Deserialize, PartialEq, Clone)]
pub struct Colors {
    #[serde(
        deserialize_with = "deserialize_to_arr",
        default = "defaults::background"
    )]
    pub background: ColorArray,

    #[serde(
        deserialize_with = "deserialize_to_arr",
        default = "defaults::foreground"
    )]
    pub foreground: ColorArray,
    #[serde(deserialize_with = "deserialize_to_arr", default = "defaults::blue")]
    pub blue: ColorArray,
    #[serde(deserialize_with = "deserialize_to_arr", default = "defaults::green")]
    pub green: ColorArray,
    #[serde(deserialize_with = "deserialize_to_arr", default = "defaults::red")]
    pub red: ColorArray,
    #[serde(deserialize_with = "deserialize_to_arr", default = "defaults::yellow")]
    pub yellow: ColorArray,
    #[serde(default = "defaults::cursor", deserialize_with = "deserialize_to_arr")]
    pub cursor: ColorArray,
    #[serde(default = "defaults::black", deserialize_with = "deserialize_to_arr")]
    pub black: ColorArray,
    #[serde(default = "defaults::cyan", deserialize_with = "deserialize_to_arr")]
    pub cyan: ColorArray,
    #[serde(default = "defaults::magenta", deserialize_with = "deserialize_to_arr")]
    pub magenta: ColorArray,
    #[serde(default = "defaults::tabs", deserialize_with = "deserialize_to_arr")]
    pub tabs: ColorArray,
    #[serde(default = "defaults::bar", deserialize_with = "deserialize_to_arr")]
    pub bar: ColorArray,
    #[serde(default = "defaults::white", deserialize_with = "deserialize_to_arr")]
    pub white: ColorArray,
    #[serde(
        default = "Option::default",
        deserialize_with = "deserialize_to_arr_opt",
        rename = "dim-black"
    )]
    pub dim_black: Option<ColorArray>,
    #[serde(
        default = "Option::default",
        deserialize_with = "deserialize_to_arr_opt",
        rename = "dim-blue"
    )]
    pub dim_blue: Option<ColorArray>,
    #[serde(
        default = "Option::default",
        deserialize_with = "deserialize_to_arr_opt",
        rename = "dim-cyan"
    )]
    pub dim_cyan: Option<ColorArray>,
    #[serde(
        default = "Option::default",
        deserialize_with = "deserialize_to_arr_opt",
        rename = "dim-foreground"
    )]
    pub dim_foreground: Option<ColorArray>,
    #[serde(
        default = "Option::default",
        deserialize_with = "deserialize_to_arr_opt",
        rename = "dim-green"
    )]
    pub dim_green: Option<ColorArray>,
    #[serde(
        default = "Option::default",
        deserialize_with = "deserialize_to_arr_opt",
        rename = "dim-magenta"
    )]
    pub dim_magenta: Option<ColorArray>,
    #[serde(
        default = "Option::default",
        deserialize_with = "deserialize_to_arr_opt",
        rename = "dim-red"
    )]
    pub dim_red: Option<ColorArray>,
    #[serde(
        default = "Option::default",
        deserialize_with = "deserialize_to_arr_opt",
        rename = "dim-white"
    )]
    pub dim_white: Option<ColorArray>,
    #[serde(
        default = "Option::default",
        deserialize_with = "deserialize_to_arr_opt",
        rename = "dim-yellow"
    )]
    pub dim_yellow: Option<ColorArray>,
    #[serde(
        default = "default_light_black",
        deserialize_with = "deserialize_to_arr",
        rename = "light-black"
    )]
    pub light_black: ColorArray,
    #[serde(
        default = "default_light_blue",
        deserialize_with = "deserialize_to_arr",
        rename = "light-blue"
    )]
    pub light_blue: ColorArray,
    #[serde(
        default = "default_light_cyan",
        deserialize_with = "deserialize_to_arr",
        rename = "light-cyan"
    )]
    pub light_cyan: ColorArray,
    #[serde(
        default = "Option::default",
        deserialize_with = "deserialize_to_arr_opt",
        rename = "light-foreground"
    )]
    pub light_foreground: Option<ColorArray>,
    #[serde(
        default = "default_light_green",
        deserialize_with = "deserialize_to_arr",
        rename = "light-green"
    )]
    pub light_green: ColorArray,
    #[serde(
        default = "default_light_magenta",
        deserialize_with = "deserialize_to_arr",
        rename = "light-magenta"
    )]
    pub light_magenta: ColorArray,
    #[serde(
        default = "default_light_red",
        deserialize_with = "deserialize_to_arr",
        rename = "light-red"
    )]
    pub light_red: ColorArray,
    #[serde(
        default = "default_light_white",
        deserialize_with = "deserialize_to_arr",
        rename = "light-white"
    )]
    pub light_white: ColorArray,
    #[serde(
        default = "default_light_yellow",
        deserialize_with = "deserialize_to_arr",
        rename = "light-yellow"
    )]
    pub light_yellow: ColorArray,
    #[serde(
        default = "defaults::selection_background",
        deserialize_with = "deserialize_to_arr",
        rename = "selection-background"
    )]
    pub selection_background: ColorArray,
    #[serde(default = "defaults::split", deserialize_with = "deserialize_to_arr")]
    pub split: ColorArray,
}

impl Default for Colors {
    fn default() -> Colors {
        Colors {
            background: defaults::background(),
            foreground: defaults::foreground(),
            blue: defaults::blue(),
            green: defaults::green(),
            red: defaults::red(),
            yellow: defaults::yellow(),
            bar: defaults::bar(),
            tabs: defaults::tabs(),
            cursor: defaults::cursor(),
            split: defaults::split(),
            black: defaults::black(),
            cyan: defaults::cyan(),
            magenta: defaults::magenta(),
            white: defaults::white(),
            dim_black: None,
            dim_blue: None,
            dim_cyan: None,
            dim_foreground: None,
            dim_green: None,
            dim_magenta: None,
            dim_red: None,
            dim_white: None,
            dim_yellow: None,
            light_black: default_light_black(),
            light_blue: default_light_blue(),
            light_cyan: default_light_cyan(),
            light_foreground: None,
            light_green: default_light_green(),
            light_magenta: default_light_magenta(),
            light_red: default_light_red(),
            light_white: default_light_white(),
            light_yellow: default_light_yellow(),
            selection_background: defaults::selection_background(),
        }
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, PartialOrd, Ord, Hash)]
pub enum NamedColor {
    Black = 0,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    White,
    LightBlack,
    LightRed,
    LightGreen,
    LightYellow,
    LightBlue,
    LightMagenta,
    LightCyan,
    LightWhite,
    Foreground = 256,
    Background,
    Cursor,
    DimBlack,
    DimRed,
    DimGreen,
    DimYellow,
    DimBlue,
    DimMagenta,
    DimCyan,
    DimWhite,
    LightForeground,
    DimForeground,
}

impl NamedColor {
    #[must_use]
    pub fn to_light(self) -> Self {
        match self {
            NamedColor::Foreground => NamedColor::LightForeground,
            NamedColor::Black => NamedColor::LightBlack,
            NamedColor::Red => NamedColor::LightRed,
            NamedColor::Green => NamedColor::LightGreen,
            NamedColor::Yellow => NamedColor::LightYellow,
            NamedColor::Blue => NamedColor::LightBlue,
            NamedColor::Magenta => NamedColor::LightMagenta,
            NamedColor::Cyan => NamedColor::LightCyan,
            NamedColor::White => NamedColor::LightWhite,
            NamedColor::DimForeground => NamedColor::Foreground,
            NamedColor::DimBlack => NamedColor::Black,
            NamedColor::DimRed => NamedColor::Red,
            NamedColor::DimGreen => NamedColor::Green,
            NamedColor::DimYellow => NamedColor::Yellow,
            NamedColor::DimBlue => NamedColor::Blue,
            NamedColor::DimMagenta => NamedColor::Magenta,
            NamedColor::DimCyan => NamedColor::Cyan,
            NamedColor::DimWhite => NamedColor::White,
            val => val,
        }
    }

    #[must_use]
    pub fn to_dim(self) -> Self {
        match self {
            NamedColor::Black => NamedColor::DimBlack,
            NamedColor::Red => NamedColor::DimRed,
            NamedColor::Green => NamedColor::DimGreen,
            NamedColor::Yellow => NamedColor::DimYellow,
            NamedColor::Blue => NamedColor::DimBlue,
            NamedColor::Magenta => NamedColor::DimMagenta,
            NamedColor::Cyan => NamedColor::DimCyan,
            NamedColor::White => NamedColor::DimWhite,
            NamedColor::Foreground => NamedColor::DimForeground,
            NamedColor::LightBlack => NamedColor::Black,
            NamedColor::LightRed => NamedColor::Red,
            NamedColor::LightGreen => NamedColor::Green,
            NamedColor::LightYellow => NamedColor::Yellow,
            NamedColor::LightBlue => NamedColor::Blue,
            NamedColor::LightMagenta => NamedColor::Magenta,
            NamedColor::LightCyan => NamedColor::Cyan,
            NamedColor::LightWhite => NamedColor::White,
            NamedColor::LightForeground => NamedColor::Foreground,
            val => val,
        }
    }
}

#[derive(Debug, PartialEq, Serialize, Deserialize, Clone, Copy)]
pub struct Rgba {
    pub red: f64,
    pub green: f64,
    pub blue: f64,
    pub alpha: f64,
}

impl Rgba {
    pub fn from_hex(hex: String) -> Result<Self, String> {
        let hex = hex.strip_prefix('#').unwrap_or(&hex);

        if !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("Error: Character is not valid".into());
        }

        if !matches!(hex.len(), 6 | 8) {
            return Err("Error: Hex String size is not valid".into());
        }

        let rgba = u32::from_str_radix(hex, 16).map_err(|error| error.to_string())?;

        let rgba = if hex.len() == 6 {
            (rgba << 8) | 255
        } else {
            rgba
        };

        let [r, g, b, a] = rgba.to_be_bytes();
        let mut color = Self::from_rgb(ColorRgb { r, g, b });

        color.alpha = f64::from(a) / 255.0;

        Ok(color)
    }

    pub fn from_rgb(rgb: ColorRgb) -> Self {
        Self {
            red: (rgb.r as f64) / 255.0,
            green: (rgb.g as f64) / 255.0,
            blue: (rgb.b as f64) / 255.0,
            alpha: 1.0,
        }
    }
}

impl Default for Rgba {
    // #000000 Color Hex Black #000
    fn default() -> Self {
        Self {
            red: 0.0,
            green: 0.0,
            blue: 0.0,
            alpha: 1.0,
        }
    }
}

impl fmt::Display for Rgba {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> Result<(), fmt::Error> {
        let text: String = self.into();

        fmt::Display::fmt(&text, f)
    }
}

pub fn deserialize_to_arr<'de, D>(deserializer: D) -> Result<ColorArray, D::Error>
where
    D: de::Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;

    match Rgba::from_hex(s) {
        Ok(color) => Ok(color.into()),
        Err(e) => Err(DeError::custom(e)),
    }
}

pub fn deserialize_to_arr_opt<'de, D>(deserializer: D) -> Result<Option<ColorArray>, D::Error>
where
    D: de::Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;

    match Rgba::from_hex(s) {
        Ok(color) => Ok(Some(color.into())),
        Err(e) => Err(DeError::custom(e)),
    }
}

impl From<ColorArray> for ColorRgb {
    fn from(arr: ColorArray) -> Self {
        // Clamp + round (instead of truncating) so a channel like 0.9999
        // maps back to 255 on float→u8 round-trips.
        let channel = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;

        ColorRgb {
            r: channel(arr[0]),
            g: channel(arr[1]),
            b: channel(arr[2]),
        }
    }
}

impl From<ColorRgb> for u32 {
    /// Packed `0xRRGGBB` form, e.g. for `gpui::rgb`.
    fn from(value: ColorRgb) -> Self {
        ((value.r as u32) << 16) | ((value.g as u32) << 8) | value.b as u32
    }
}

impl From<&Rgba> for ColorArray {
    fn from(value: &Rgba) -> Self {
        [
            value.red as f32,
            value.green as f32,
            value.blue as f32,
            value.alpha as f32,
        ]
    }
}

impl From<ColorRgb> for ColorArray {
    fn from(value: ColorRgb) -> Self {
        (&value).into()
    }
}

impl From<Rgba> for ColorArray {
    fn from(value: Rgba) -> Self {
        (&value).into()
    }
}

impl From<u32> for ColorRgb {
    /// Decode packed `0xRRGGBB` channels, ignoring the high byte.
    fn from(id: u32) -> Self {
        ColorRgb {
            r: ((id >> 16) & 0xFF) as u8,
            g: ((id >> 8) & 0xFF) as u8,
            b: (id & 0xFF) as u8,
        }
    }
}

impl From<&Rgba> for String {
    fn from(value: &Rgba) -> Self {
        format!(
            "r: {:?}, g: {:?}, b: {:?}, a: {:?}",
            value.red, value.green, value.blue, value.alpha
        )
    }
}

#[cfg(test)]
mod tests {
    use crate::colors::Rgba;

    #[test]
    fn hex_colors_accept_rgb_and_rgba_and_reject_non_ascii_digits() {
        for input in [
            "12345٣", "#12٣456", "#12345", "##123456", "+12345", "#GG0000",
        ] {
            assert!(Rgba::from_hex(input.into()).is_err(), "{input}");
        }

        let rgb = Rgba::from_hex("#ff8040".into()).unwrap();

        assert_eq!(
            (rgb.red, rgb.green, rgb.blue, rgb.alpha),
            (1.0, 128.0 / 255.0, 64.0 / 255.0, 1.0)
        );

        let rgba = Rgba::from_hex("FF804020".into()).unwrap();

        assert_eq!(
            (rgba.red, rgba.green, rgba.blue, rgba.alpha),
            (1.0, 128.0 / 255.0, 64.0 / 255.0, 32.0 / 255.0)
        );
    }
}
