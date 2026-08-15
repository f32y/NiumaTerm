use crate::{CursorShape, Shell};

#[inline]
pub fn default_bool_true() -> bool {
    true
}

#[inline]
pub fn default_cursor_interval() -> u64 {
    800
}

#[inline]
pub fn default_shell() -> Shell {
    #[cfg(not(target_os = "windows"))]
    {
        Shell {
            program: String::from(""),
            args: vec![String::from("--login")],
        }
    }

    #[cfg(target_os = "windows")]
    {
        Shell {
            program: String::from("powershell"),
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
    String::from("modern_dark")
}

#[inline]
pub fn default_editor() -> Shell {
    #[cfg(not(target_os = "windows"))]
    {
        Shell {
            program: String::from("vi"),
            args: vec![],
        }
    }

    #[cfg(target_os = "windows")]
    {
        Shell {
            program: String::from("notepad"),
            args: vec![],
        }
    }
}
