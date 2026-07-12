use serde::{Deserialize, Serialize};

// Examples:
// { key = "w", mods: "super", action = "quit" }
// { key = "Home", mods: "super | shift", esc = "\x1b[5~" }

#[derive(Debug, PartialEq, Clone, Serialize, Deserialize)]
pub struct KeyBinding {
    pub key: String,
    #[serde(default = "String::default")]
    pub with: String,
    #[serde(default = "String::default")]
    pub action: String,
    #[serde(default = "String::default")]
    pub esc: String,
    #[serde(default = "String::default")]
    pub mode: String,
}

pub type KeyBindings = Vec<KeyBinding>;

#[derive(Default, Debug, PartialEq, Clone, Serialize, Deserialize)]
pub struct Bindings {
    pub keys: KeyBindings,
}
