use serde::{Deserialize, Serialize};
use toml::Value;

use crate::colors::Colors;

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
