use std::{env, io};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

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

/// Build a Windows hook command that remains valid when an agent executes it
/// through either cmd.exe or PowerShell.
pub fn build_windows_hook_command(executable: &str, argument: &str) -> io::Result<String> {
    let system_root = env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());

    build_windows_hook_command_for(executable, argument, &system_root)
}

pub(super) fn build_windows_hook_command_for(
    executable: &str,
    argument: &str,
    system_root: &str,
) -> io::Result<String> {
    if executable.is_empty() || executable.contains(['\0', '\r', '\n']) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "hook executable path is invalid",
        ));
    }

    if argument.is_empty()
        || !argument
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "hook argument is not shell-safe",
        ));
    }

    // A bare path containing only cmd/PowerShell-safe characters starts
    // without another interpreter and avoids per-event PowerShell startup.
    if executable.bytes().all(|byte| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b':' | b'\\' | b'~' | b'-')
    }) {
        return Ok(format!("{executable} {argument}"));
    }

    // Spaces and cmd metacharacters such as `%` and `^` cannot be made safe by
    // quoting alone. Encoding the PowerShell invocation keeps the executable
    // path out of the outer agent shell's parser.
    let quoted = executable.replace('\'', "''");
    let script = format!("& '{quoted}' {argument}; exit $LASTEXITCODE");

    let encoded = STANDARD.encode(
        script
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>(),
    );

    let powershell = format!(
        "{}/System32/WindowsPowerShell/v1.0/powershell.exe",
        system_root.trim_end_matches(['\\', '/']).replace('\\', "/")
    );

    Ok(format!(
        "{powershell} -NoProfile -ExecutionPolicy Bypass -EncodedCommand {encoded}"
    ))
}

/// Match a marker in either a plain hook command or the decoded payload of a
/// PowerShell `-EncodedCommand` launcher.
pub fn hook_command_contains(command: &str, marker: &str) -> bool {
    command.contains(marker)
        || decode_powershell_command(command).is_some_and(|decoded| decoded.contains(marker))
}

fn decode_powershell_command(command: &str) -> Option<String> {
    let mut parts = command.split_whitespace();

    let encoded = loop {
        if parts.next()?.eq_ignore_ascii_case("-EncodedCommand") {
            break parts.next()?;
        }
    };

    let bytes = STANDARD.decode(encoded).ok()?;

    let mut chunks = bytes.chunks_exact(2);

    let units = chunks
        .by_ref()
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .collect::<Vec<_>>();

    if !chunks.remainder().is_empty() {
        return None;
    }

    String::from_utf16(&units).ok()
}
