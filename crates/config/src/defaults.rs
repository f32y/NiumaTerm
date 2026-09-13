use nmt_platform::environment::DEFAULT_EDITOR;
#[cfg(target_os = "windows")]
use nmt_platform::windows::powershell::DEFAULT_CONFIG_SHELL;

use crate::{CursorShape, Shell};

#[inline]
pub fn default_bool_true() -> bool {
    true
}

#[inline]
pub fn default_shell() -> Shell {
    #[cfg(not(target_os = "windows"))]
    {
        Shell {
            program: "".into(),
            args: vec!["--login".into()],
        }
    }

    #[cfg(target_os = "windows")]
    {
        Shell {
            program: DEFAULT_CONFIG_SHELL.into(),
            args: vec![],
        }
    }
}

#[inline]
pub fn default_working_dir() -> Option<String> {
    None
}

#[inline]
pub fn default_cursor() -> CursorShape {
    CursorShape::default()
}

#[inline]
pub fn default_theme() -> String {
    "modern_dark".into()
}

#[inline]
pub fn default_editor() -> Shell {
    Shell {
        program: DEFAULT_EDITOR.into(),
        args: vec![],
    }
}
