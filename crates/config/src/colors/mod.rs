pub mod defaults;
pub mod term;

use std::fmt;
use std::num::ParseIntError;
use std::ops::Mul;
use std::sync::OnceLock;

use regex::Regex;
use serde::de::Error as DeError;
use serde::{Deserialize, Serialize, de};
use tracing::trace;

use crate::colors::defaults::*;
use crate::render_types;

// `ColorWGPU` is the legacy name; `crate::render_types::Color` is the actual
// type now (mirrors `wgpu::Color`'s shape, but doesn't drag wgpu
// into the dep tree on Linux/macOS native builds).
pub type ColorWGPU = render_types::Color;

pub type ColorArray = [f32; 4];

pub type ColorComposition = (ColorArray, ColorWGPU);

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
        ColorBuilder::from_rgb(*color, Format::SRGB0_1).into()
    }
}

impl From<(u8, u8, u8)> for ColorRgb {
    fn from((r, g, b): (u8, u8, u8)) -> Self {
        Self { r, g, b }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum Format {
    SRGB0_255,
    SRGB0_1,
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
        deserialize_with = "deserialize_to_composition",
        default = "defaults::background"
    )]
    /// Background is a special color type called ColorComposition
    /// ColorComposition type is (ColorArray, ColorWGPU)
    /// See more in colors definition
    pub background: ColorComposition,

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
    #[serde(
        deserialize_with = "deserialize_to_arr",
        default = "defaults::tabs_active",
        rename = "tabs-active"
    )]
    pub tabs_active: ColorArray,
    #[serde(default = "defaults::cursor", deserialize_with = "deserialize_to_arr")]
    pub cursor: ColorArray,
    #[serde(
        default = "defaults::vi_cursor",
        rename = "vi-cursor",
        deserialize_with = "deserialize_to_arr"
    )]
    pub vi_cursor: ColorArray,
    #[serde(default = "defaults::black", deserialize_with = "deserialize_to_arr")]
    pub black: ColorArray,
    #[serde(default = "defaults::cyan", deserialize_with = "deserialize_to_arr")]
    pub cyan: ColorArray,
    #[serde(default = "defaults::magenta", deserialize_with = "deserialize_to_arr")]
    pub magenta: ColorArray,
    #[serde(default = "defaults::tabs", deserialize_with = "deserialize_to_arr")]
    pub tabs: ColorArray,
    #[serde(
        default = "defaults::tab_border",
        rename = "tab-border",
        deserialize_with = "deserialize_to_arr"
    )]
    pub tab_border: ColorArray,
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
    #[serde(
        default = "defaults::selection_foreground",
        deserialize_with = "deserialize_to_arr",
        rename = "selection-foreground"
    )]
    pub selection_foreground: ColorArray,
    #[serde(default = "defaults::split", deserialize_with = "deserialize_to_arr")]
    pub split: ColorArray,
    #[serde(
        default = "defaults::split_active",
        deserialize_with = "deserialize_to_arr",
        rename = "split-active"
    )]
    pub split_active: ColorArray,
    #[serde(
        default = "defaults::search_match_background",
        deserialize_with = "deserialize_to_arr",
        rename = "search-match-background"
    )]
    pub search_match_background: ColorArray,
    #[serde(
        default = "defaults::search_match_foreground",
        deserialize_with = "deserialize_to_arr",
        rename = "search-match-foreground"
    )]
    pub search_match_foreground: ColorArray,
    #[serde(
        default = "defaults::search_focused_match_background",
        deserialize_with = "deserialize_to_arr",
        rename = "search-focused-match-background"
    )]
    pub search_focused_match_background: ColorArray,
    #[serde(
        default = "defaults::search_focused_match_foreground",
        deserialize_with = "deserialize_to_arr",
        rename = "search-focused-match-foreground"
    )]
    pub search_focused_match_foreground: ColorArray,
    #[serde(
        default = "defaults::hint_foreground",
        deserialize_with = "deserialize_to_arr",
        rename = "hint-foreground"
    )]
    pub hint_foreground: ColorArray,
    #[serde(
        default = "defaults::hint_background",
        deserialize_with = "deserialize_to_arr",
        rename = "hint-background"
    )]
    pub hint_background: ColorArray,
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
            tabs_active: defaults::tabs_active(),
            tab_border: defaults::tab_border(),
            cursor: defaults::cursor(),
            split: defaults::split(),
            split_active: defaults::split_active(),
            vi_cursor: defaults::vi_cursor(),
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
            selection_foreground: defaults::selection_foreground(),
            search_match_background: defaults::search_match_background(),
            search_match_foreground: defaults::search_match_foreground(),
            search_focused_match_background: defaults::search_focused_match_background(),
            search_focused_match_foreground: defaults::search_focused_match_foreground(),
            hint_foreground: defaults::hint_foreground(),
            hint_background: defaults::hint_background(),
        }
    }
}

pub fn hex_to_color_arr(s: &str) -> ColorArray {
    ColorBuilder::from_hex(s.to_string(), Format::SRGB0_1)
        .unwrap_or_default()
        .into()
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
pub struct ColorBuilder {
    pub red: f64,
    pub green: f64,
    pub blue: f64,
    pub alpha: f64,
}

impl ColorBuilder {
    pub fn from_hex(mut hex: String, conversion_type: Format) -> Result<Self, String> {
        // Compiled once: this runs for every color of every theme, and regex
        // compilation dwarfs the match itself.
        static NON_HEX_CHARS: OnceLock<Regex> = OnceLock::new();

        static VALID_HEX_SIZE: OnceLock<Regex> = OnceLock::new();

        let mut alpha: f64 = 1.0;
        let non_hex_chars = NON_HEX_CHARS.get_or_init(|| Regex::new(r"(?i)[^#a-f\d]").unwrap());

        // match valid 6 or 8 hex characters
        let valid_hex_size =
            VALID_HEX_SIZE.get_or_init(|| Regex::new(r"(?i)^#?[a-f\d]{6}([a-f\d]{2})?$").unwrap());

        if non_hex_chars.is_match(&hex) {
            return Err("Error: Character is not valid".into());
        }

        if !valid_hex_size.is_match(&hex) {
            return Err("Error: Hex String size is not valid".into());
        }

        hex = hex.replace('#', "");

        if hex.len() == 8 {
            let (rgb_part, alpha_part) = hex.split_at(6);
            let alpha_from_hex = i32::from_str_radix(alpha_part, 16).unwrap();

            hex = rgb_part.to_string();
            alpha = (alpha_from_hex as f64) / 255.0;
        }

        let rgb = decode_hex(&hex).unwrap_or_default();

        if rgb.is_empty() || (rgb.len() != 3 && rgb.len() != 4) {
            return Err("Error: Invalid string, not able to convert".into());
        }

        match conversion_type {
            Format::SRGB0_1 => Ok(Self {
                red: (rgb[0] as f64) / 255.0,
                green: (rgb[1] as f64) / 255.0,
                blue: (rgb[2] as f64) / 255.0,
                alpha,
            }),

            Format::SRGB0_255 => Ok(Self {
                red: (rgb[0] as f64),
                green: (rgb[1] as f64),
                blue: (rgb[2] as f64),
                alpha,
            }),
        }
    }

    pub fn from_rgb(rgb: ColorRgb, conversion_type: Format) -> Self {
        match conversion_type {
            Format::SRGB0_1 => Self {
                red: (rgb.r as f64) / 255.0,
                green: (rgb.g as f64) / 255.0,
                blue: (rgb.b as f64) / 255.0,
                alpha: 1.0,
            },

            Format::SRGB0_255 => Self {
                red: (rgb.r as f64),
                green: (rgb.g as f64),
                blue: (rgb.b as f64),
                alpha: 1.0,
            },
        }
    }
}

fn decode_hex(s: &str) -> Result<Vec<u8>, ParseIntError> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16))
        .collect()
}

impl Default for ColorBuilder {
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

impl fmt::Display for ColorBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> Result<(), fmt::Error> {
        let text: String = self.into();

        fmt::Display::fmt(&text, f)
    }
}

pub fn deserialize_to_composition<'de, D>(deserializer: D) -> Result<ColorComposition, D::Error>
where
    D: de::Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;

    match ColorBuilder::from_hex(s, Format::SRGB0_1) {
        Ok(color) => Ok((color.into(), color.into())),
        Err(e) => Err(DeError::custom(e)),
    }
}

pub fn deserialize_to_arr<'de, D>(deserializer: D) -> Result<ColorArray, D::Error>
where
    D: de::Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;

    match ColorBuilder::from_hex(s, Format::SRGB0_1) {
        Ok(color) => Ok(color.into()),
        Err(e) => Err(DeError::custom(e)),
    }
}

pub fn deserialize_to_arr_opt<'de, D>(deserializer: D) -> Result<Option<ColorArray>, D::Error>
where
    D: de::Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;

    match ColorBuilder::from_hex(s, Format::SRGB0_1) {
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

impl From<&ColorRgb> for ColorWGPU {
    fn from(value: &ColorRgb) -> Self {
        ColorBuilder::from_rgb(*value, Format::SRGB0_1).into()
    }
}

impl From<&ColorBuilder> for ColorArray {
    fn from(value: &ColorBuilder) -> Self {
        [
            value.red as f32,
            value.green as f32,
            value.blue as f32,
            value.alpha as f32,
        ]
    }
}

impl From<&ColorBuilder> for ColorWGPU {
    fn from(value: &ColorBuilder) -> Self {
        render_types::Color {
            r: value.red,
            g: value.green,
            b: value.blue,
            a: value.alpha,
        }
    }
}

impl From<ColorRgb> for ColorArray {
    fn from(value: ColorRgb) -> Self {
        (&value).into()
    }
}

impl From<ColorRgb> for ColorWGPU {
    fn from(value: ColorRgb) -> Self {
        (&value).into()
    }
}

impl From<ColorBuilder> for ColorArray {
    fn from(value: ColorBuilder) -> Self {
        (&value).into()
    }
}

impl From<ColorBuilder> for ColorWGPU {
    fn from(value: ColorBuilder) -> Self {
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

impl From<&ColorBuilder> for String {
    fn from(value: &ColorBuilder) -> Self {
        format!(
            "r: {:?}, g: {:?}, b: {:?}, a: {:?}",
            value.red, value.green, value.blue, value.alpha
        )
    }
}
