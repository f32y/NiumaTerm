use serde::{Deserialize, Serialize};
use toml::Value;

use crate::colors::Colors;

#[derive(Default, Debug, Serialize, Deserialize, PartialEq, Clone)]
pub struct AdaptiveColors {
    #[serde(default = "Option::default", skip_serializing)]
    pub dark: Option<Colors>,
    #[serde(default = "Option::default", skip_serializing)]
    pub light: Option<Colors>,
}

#[derive(Default, Debug, Serialize, Deserialize, PartialEq, Clone)]
pub struct AdaptiveTheme {
    pub dark: String,
    pub light: String,
}

#[derive(Debug, Default, Clone, Deserialize, PartialEq)]
pub struct Theme {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub mode: AppearanceTheme,
    #[serde(default)]
    pub colors: ThemeColors,
}

#[derive(Debug, Default, Clone, Deserialize, PartialEq)]
pub struct ThemeColors {
    #[serde(default = "Colors::default")]
    pub terminal: Colors,
    #[serde(default)]
    pub ui: Option<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UiTheme {
    pub name: String,
    pub mode: AppearanceTheme,
    pub colors: Value,
}

impl Theme {
    pub fn ui_theme(&self) -> Option<UiTheme> {
        self.colors.ui.clone().map(|colors| UiTheme {
            name: self.name.clone(),
            mode: self.mode,
            colors,
        })
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AppearanceTheme {
    #[default]
    Dark,
    Light,
}

impl AppearanceTheme {
    pub fn toggled(self) -> Self {
        match self {
            AppearanceTheme::Dark => AppearanceTheme::Light,
            AppearanceTheme::Light => AppearanceTheme::Dark,
        }
    }
}
