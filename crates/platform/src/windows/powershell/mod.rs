use std::path::Path;
use std::sync::OnceLock;
use std::{env, fs, io};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

pub const DEFAULT_SHELL: &str = "powershell.exe";
pub const LEGACY_SHELL: &str = r"C:\WINDOWS\System32\WindowsPowerShell\v1.0\powershell.exe";
pub const INTEGRATION_SCRIPT: &str =
    include_str!("../../../../../assets/windows/pwsh-integration.ps1");
pub const DEFAULT_CONFIG_SHELL: &str = "powershell";

pub fn is_shell(shell: Option<&str>) -> bool {
    match shell {
        Some(shell) => {
            let lower = shell.to_ascii_lowercase();
            lower.contains("powershell") || lower.contains("pwsh")
        }
        None => true,
    }
}

pub fn encode_command(script: &str) -> String {
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    STANDARD.encode(bytes)
}

pub fn newest_install(root: &Path) -> Option<String> {
    fs::read_dir(root)
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let major: u32 = entry.file_name().to_str()?.parse().ok()?;
            let executable = entry.path().join("pwsh.exe");
            (major >= 7 && executable.is_file()).then_some((major, executable))
        })
        .max_by_key(|(major, _)| *major)
        .map(|(_, executable)| executable.to_string_lossy().into_owned())
}

pub fn preferred_shell() -> &'static str {
    static SHELL: OnceLock<String> = OnceLock::new();

    SHELL.get_or_init(|| {
        let program_files = env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".into());
        newest_install(&Path::new(&program_files).join("PowerShell"))
            .or_else(|| Some(which::which("pwsh").ok()?.to_string_lossy().into_owned()))
            .unwrap_or_else(|| LEGACY_SHELL.to_string())
    })
}

pub fn build_hook_command(executable: &str, argument: &str) -> io::Result<String> {
    let system_root = env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    build_hook_command_for(executable, argument, &system_root)
}

fn build_hook_command_for(
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

    if executable.bytes().all(|byte| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b':' | b'\\' | b'~' | b'-')
    }) {
        return Ok(format!("{executable} {argument}"));
    }

    let quoted = executable.replace('\'', "''");
    let script = format!("& '{quoted}' {argument}; exit $LASTEXITCODE");
    let encoded = encode_command(&script);
    let powershell = format!(
        "{}/System32/WindowsPowerShell/v1.0/powershell.exe",
        system_root.trim_end_matches(['\\', '/']).replace('\\', "/")
    );

    Ok(format!(
        "{powershell} -NoProfile -ExecutionPolicy Bypass -EncodedCommand {encoded}"
    ))
}

pub fn hook_command_contains(command: &str, marker: &str) -> bool {
    command.contains(marker)
        || decode_command_argument(command).is_some_and(|decoded| decoded.contains(marker))
}

fn decode_command_argument(command: &str) -> Option<String> {
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

#[cfg(test)]
mod tests;
