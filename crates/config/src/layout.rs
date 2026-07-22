use serde::de::Error as DeError;
use serde::{Deserialize, Deserializer, Serialize};

// Panel configuration for split layouts
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Panel {
    #[serde(default = "default_panel_margin")]
    pub margin: Margin,
    #[serde(default = "default_panel_padding")]
    pub padding: Margin,
    #[serde(default = "default_row_gap", rename = "row-gap")]
    pub row_gap: f32,
    #[serde(default = "default_column_gap", rename = "column-gap")]
    pub column_gap: f32,
    #[serde(default = "default_border_width", rename = "border-width")]
    pub border_width: f32,
    #[serde(default = "default_border_radius", rename = "border-radius")]
    pub border_radius: f32,
}

impl Default for Panel {
    fn default() -> Self {
        Self {
            margin: default_panel_margin(),
            padding: default_panel_padding(),
            row_gap: default_row_gap(),
            column_gap: default_column_gap(),
            border_width: default_border_width(),
            border_radius: default_border_radius(),
        }
    }
}

#[inline]
fn default_panel_margin() -> Margin {
    Margin::all(2.0)
}

#[inline]
fn default_panel_padding() -> Margin {
    Margin::all(5.0)
}

#[inline]
fn default_row_gap() -> f32 {
    0.0
}

#[inline]
fn default_column_gap() -> f32 {
    0.0
}

#[inline]
fn default_border_width() -> f32 {
    2.0
}

#[inline]
fn default_border_radius() -> f32 {
    0.0
}

// CSS-like margin structure
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Margin {
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub left: f32,
}

impl Margin {
    pub fn new(top: f32, right: f32, bottom: f32, left: f32) -> Self {
        Self {
            top,
            right,
            bottom,
            left,
        }
    }

    pub fn all(value: f32) -> Self {
        Self::new(value, value, value, value)
    }

    pub fn from_css_values(values: &[f32]) -> Result<Self, String> {
        match values.len() {
            1 => Ok(Self::all(values[0])),
            2 => Ok(Self::new(values[0], values[1], values[0], values[1])),
            4 => Ok(Self::new(values[0], values[1], values[2], values[3])),
            _ => Err(format!(
                "Invalid margin format: expected 1, 2, or 4 values, got {}",
                values.len()
            )),
        }
    }
}

impl Default for Margin {
    fn default() -> Self {
        Self::all(10.0)
    }
}

impl<'de> Deserialize<'de> for Margin {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let values: Vec<f32> = Vec::deserialize(deserializer)?;
        Self::from_css_values(&values).map_err(DeError::custom)
    }
}
