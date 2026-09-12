use std::io;

/// Characters a POSIX shell passes through verbatim in an unquoted word.
///
/// `~` is deliberately absent: a leading tilde is expanded by the shell, so a
/// path that starts with one has to reach the shell quoted.
fn is_bare_word(executable: &str) -> bool {
    executable
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'/' | b'-'))
}

/// The command line an agent's hook config runs to invoke `executable` with a
/// single `argument`.
///
/// Agents execute these entries through a shell, so a path holding spaces or
/// shell metacharacters is wrapped in single quotes, which suppress every
/// expansion. A literal quote is closed, escaped, and reopened because single
/// quotes do not nest.
pub fn build_hook_command(executable: &str, argument: &str) -> io::Result<String> {
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

    if is_bare_word(executable) {
        return Ok(format!("{executable} {argument}"));
    }

    Ok(format!("{} {argument}", single_quoted(executable)))
}

/// `value` as one POSIX shell word.
///
/// Single quotes suppress every expansion; a literal quote is closed, escaped
/// and reopened because single quotes do not nest.
pub(super) fn single_quoted(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// Whether `command` invokes the binary identified by `marker`.
///
/// A POSIX hook command always carries the executable path literally, so
/// unlike the PowerShell form there is no encoded payload to decode first.
pub fn hook_command_contains(command: &str, marker: &str) -> bool {
    command.contains(marker)
}

#[cfg(test)]
#[path = "hook_command_tests.rs"]
mod hook_command_tests;
