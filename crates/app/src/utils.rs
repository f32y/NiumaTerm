use std::path::{Path, PathBuf};

/// The NiumaTerm per-user data directory: `%LOCALAPPDATA%\NiumaTerm`, falling back to
/// `%TEMP%` if `LOCALAPPDATA` is unset or uncreatable.
pub(crate) fn get_data_dir() -> PathBuf {
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        let dir = Path::new(&local).join("NiumaTerm");
        if std::fs::create_dir_all(&dir).is_ok() {
            return dir;
        }
    }
    std::env::temp_dir()
}

pub(crate) fn get_exe_dir() -> PathBuf {
    std::env::current_exe()
        .expect("locate current executable")
        .parent()
        .expect("current executable has no parent")
        .to_path_buf()
}

pub(crate) fn get_assets_path() -> PathBuf {
    get_exe_dir().join("assets")
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ShellKind {
    #[cfg(test)]
    Bash,
    #[cfg(test)]
    PowerShellOld,
    PowerShell,
}

impl ShellKind {
    /// Classify a shell command or path into a known shell kind. `pwsh` is
    /// PowerShell 7+, `powershell` is Windows PowerShell 5. Anything containing
    /// `bash` is Bash; unknown shells fall back to [`ShellKind::PowerShell`], the
    /// app default.
    #[cfg(test)]
    pub fn from_command(shell: &str) -> ShellKind {
        let lower = shell.to_ascii_lowercase();
        if lower.contains("pwsh") {
            ShellKind::PowerShell
        } else if lower.contains("powershell") {
            ShellKind::PowerShellOld
        } else if lower.contains("bash") {
            ShellKind::Bash
        } else {
            ShellKind::PowerShell
        }
    }

    /// The tree-sitter language name for this shell. Both PowerShell editions
    /// share the `powershell` grammar.
    /// (Requires the matching `tree-sitter-*` feature; an unregistered language
    /// degrades to plain text.)
    #[cfg(test)]
    pub fn language(self) -> &'static str {
        match self {
            ShellKind::Bash => "bash",
            ShellKind::PowerShell | ShellKind::PowerShellOld => "powershell",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ShellKind;

    #[test]
    fn detects_shell_kind_and_language() {
        assert_eq!(ShellKind::from_command("pwsh.exe").language(), "powershell");
        assert_eq!(
            ShellKind::from_command(r"C:\WINDOWS\System32\WindowsPowerShell\v1.0\powershell.exe"),
            ShellKind::PowerShellOld
        );
        assert_eq!(ShellKind::from_command("/bin/bash").language(), "bash");
        // Unknown shells fall back to PowerShell (the app default).
        assert_eq!(ShellKind::from_command("cmd.exe"), ShellKind::PowerShell);
    }
}

/// Materialize the bundled PowerShell shell-integration script to the data dir and
/// return its path. The script is compiled into the binary (`include_str!`) and
/// rewritten on each call so it tracks the shipped version. `None` if it cannot be
/// written (the caller then launches the shell without integration).
pub fn get_shell_bootstrap_script(kind: ShellKind) -> Option<PathBuf> {
    match kind {
        ShellKind::PowerShell => Some(get_assets_path().join("pwsh-integration.ps1")),
        #[cfg(test)]
        _ => None,
    }
}
