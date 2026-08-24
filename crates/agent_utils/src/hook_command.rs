pub use nmt_platform::windows::powershell::{
    build_hook_command as build_windows_hook_command, hook_command_contains,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookInstallStatus {
    /// Every event is registered with the current hook command.
    Installed,
    /// NiumaTerm entries exist but differ from the current command (for
    /// example a legacy absolute-path install) or miss events; reinstalling
    /// migrates them.
    Stale,
    NotInstalled,
}
