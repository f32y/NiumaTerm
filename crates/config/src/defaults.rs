#[cfg(feature = "application")]
use nmt_platform::environment::DEFAULT_EDITOR;

use crate::CursorShape;
#[cfg(feature = "application")]
use crate::Shell;

#[inline]
pub fn default_bool_true() -> bool {
    true
}

#[cfg(feature = "application")]
#[inline]
pub fn default_shell() -> Shell {
    let (program, args) = nmt_platform::configured_shell_defaults();

    Shell { program, args }
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

#[cfg(feature = "application")]
#[inline]
pub fn default_editor() -> Shell {
    Shell {
        program: DEFAULT_EDITOR.into(),
        args: vec![],
    }
}
