use std::{env, fs, process};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

use crate::windows::powershell::{
    build_hook_command, build_hook_command_for, hook_command_contains, newest_install,
};

#[test]
fn newest_install_picks_the_highest_supported_major() {
    let root = env::temp_dir().join("nmt-newest-pwsh-test");
    let _ = fs::remove_dir_all(&root);
    for major in ["6", "7", "9"] {
        fs::create_dir_all(root.join(major)).unwrap();
    }
    for major in ["6", "7"] {
        fs::write(root.join(major).join("pwsh.exe"), "").unwrap();
    }

    assert_eq!(
        newest_install(&root),
        Some(
            root.join("7")
                .join("pwsh.exe")
                .to_string_lossy()
                .into_owned()
        )
    );

    fs::remove_dir_all(&root).unwrap();
    assert_eq!(newest_install(&root), None);
}

#[test]
fn hook_command_uses_bare_safe_path() {
    assert_eq!(
        build_hook_command_for(
            r"C:\Soft\NiumaTerm\NmtAgentHook.exe",
            "codex",
            r"C:\Windows",
        )
        .unwrap(),
        r"C:\Soft\NiumaTerm\NmtAgentHook.exe codex"
    );
    assert!(build_hook_command_for(r"C:\Hook.exe", "codex & whoami", r"C:\Windows").is_err());
}

#[test]
fn hook_command_encodes_unsafe_path() {
    let executable = r"C:\Program Files\Niuma'Term\%HOOK%^\NmtAgentHook.exe";
    let command = build_hook_command_for(executable, "codex", r"D:\Windows").unwrap();
    assert!(command.starts_with(
        "D:/Windows/System32/WindowsPowerShell/v1.0/powershell.exe -NoProfile \
         -ExecutionPolicy Bypass -EncodedCommand "
    ));
    assert!(!command.contains(executable));
    assert!(hook_command_contains(&command, "NmtAgentHook.exe"));

    let encoded = command.rsplit_once(' ').unwrap().1;
    let bytes = STANDARD.decode(encoded).unwrap();
    let units = bytes
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .collect::<Vec<_>>();
    assert_eq!(
        String::from_utf16(&units).unwrap(),
        r"& 'C:\Program Files\Niuma''Term\%HOOK%^\NmtAgentHook.exe' codex; exit $LASTEXITCODE"
    );
}

#[test]
fn generated_hook_command_runs_in_both_windows_shells() {
    let directory = env::temp_dir().join(format!("nmt hook path % ^ {}", process::id()));
    fs::create_dir_all(&directory).unwrap();
    let script = directory.join("hook.cmd");
    fs::write(&script, "@echo hook-ran\r\n@exit /b 0\r\n").unwrap();
    let command = build_hook_command(script.to_str().unwrap(), "codex").unwrap();

    for (shell, args) in [
        ("powershell.exe", vec!["-NoProfile", "-Command", &command]),
        ("cmd.exe", vec!["/d", "/c", &command]),
    ] {
        let output = process::Command::new(shell).args(args).output().unwrap();
        assert_eq!(
            output.status.code(),
            Some(0),
            "{shell}: stdout={}, stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("hook-ran"));
    }

    fs::remove_dir_all(directory).unwrap();
}
