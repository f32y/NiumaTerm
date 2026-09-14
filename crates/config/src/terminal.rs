use serde::{Deserialize, Serialize};

use crate::defaults::default_bool_true;

/// Terminal behavior persisted independently of visual settings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct TerminalConfig {
    pub improve_powershell_compatibility: bool,
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            improve_powershell_compatibility: default_bool_true(),
        }
    }
}
