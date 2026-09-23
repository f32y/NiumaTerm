use std::{env, fs, process};

use crate::windows::{command_line, quote_command_arg};

#[test]
fn command_line_quotes_shell_path_and_args() {
    let args = vec![
        "-NoExit".to_string(),
        "-Command".to_string(),
        r". 'C:\Program Files\NiumaTerm\assets\shell-integrations\nmt-integration.ps1'".to_string(),
    ];

    assert_eq!(
        command_line(r"C:\Program Files\PowerShell\7\pwsh.exe", &args),
        r#""C:\Program Files\PowerShell\7\pwsh.exe" -NoExit -Command ". 'C:\Program Files\NiumaTerm\assets\shell-integrations\nmt-integration.ps1'""#
    );
}

#[test]
fn command_line_keeps_legacy_raw_shell_when_args_are_empty() {
    assert_eq!(
        command_line("powershell -NoProfile -Command echo", &[]),
        "powershell -NoProfile -Command echo"
    );
}

#[test]
fn command_line_quotes_an_existing_shell_path_without_args() {
    let directory = env::temp_dir().join(format!("niumaterm shell {}", process::id()));

    fs::create_dir_all(&directory).unwrap();

    let shell = directory.join("my shell.exe");

    fs::write(&shell, b"").unwrap();

    let line = command_line(shell.to_str().unwrap(), &[]);

    fs::remove_dir_all(&directory).unwrap();

    assert_eq!(line, format!("\"{}\"", shell.display()));
}

#[test]
fn quote_command_arg_handles_windows_argv_rules() {
    assert_eq!(quote_command_arg("pwsh.exe"), "pwsh.exe");
    assert_eq!(quote_command_arg(""), r#""""#);
    assert_eq!(
        quote_command_arg(r#"a "quoted" arg\"#),
        r#""a \"quoted\" arg\\""#
    );
}
