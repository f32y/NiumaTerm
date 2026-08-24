use std::{env, fs, io, mem, process, ptr};

use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::System::Threading::{
    CREATE_UNICODE_ENVIRONMENT, CreateProcessW, INFINITE, PROCESS_INFORMATION, STARTUPINFOW,
    WaitForSingleObject,
};

use crate::windows::conpty::build_environment_block;

fn entries(block: &[u16]) -> Vec<String> {
    block[..block.len() - 1]
        .split(|unit| *unit == 0)
        .filter(|entry| !entry.is_empty())
        .map(String::from_utf16_lossy)
        .collect()
}

#[test]
fn overrides_replace_names_case_insensitively_without_mutating_parent() {
    let key = "NMT_PTY_ENV_REPLACEMENT_TEST";
    unsafe { env::set_var(key, "parent") };
    let block = build_environment_block(&[(key.to_lowercase(), "child".into())]);
    let matching: Vec<_> = entries(&block)
        .into_iter()
        .filter(|entry| entry.to_lowercase().starts_with(&key.to_lowercase()))
        .collect();
    assert_eq!(matching, [format!("{}=child", key.to_lowercase())]);
    assert_eq!(env::var(key).as_deref(), Ok("parent"));
    unsafe { env::remove_var(key) };
}

#[test]
fn block_preserves_unrelated_values_and_unicode() {
    let inherited = "NMT_PTY_ENV_PRESERVE_TEST";
    unsafe { env::set_var(inherited, "kept") };
    let block = build_environment_block(&[("NMT_UNICODE".into(), "牛马终端🦀".into())]);
    let entries = entries(&block);
    assert!(
        entries
            .iter()
            .any(|entry| entry == &format!("{inherited}=kept"))
    );
    assert!(
        entries
            .iter()
            .any(|entry| entry == "NMT_UNICODE=牛马终端🦀")
    );
    unsafe { env::remove_var(inherited) };
}

#[test]
fn block_advertises_truecolor_by_default() {
    let block = build_environment_block(&[]);
    assert!(
        entries(&block)
            .iter()
            .any(|entry| entry.eq_ignore_ascii_case("COLORTERM=truecolor"))
    );
}

#[test]
fn block_advertises_terminal_progress_by_default() {
    let block = build_environment_block(&[]);
    assert!(entries(&block).iter().any(|entry| {
        entry.split_once('=').is_some_and(|(key, value)| {
            key.eq_ignore_ascii_case("TERM_FEATURES") && value.contains('P')
        })
    }));
}

#[test]
fn block_is_sorted_case_insensitively_and_double_nul_terminated() {
    let block =
        build_environment_block(&[("zz_nmt".into(), "z".into()), ("AA_NMT".into(), "a".into())]);
    assert!(block.len() >= 2);
    assert_eq!(&block[block.len() - 2..], &[0, 0]);
    let entries = entries(&block);
    let folded: Vec<_> = entries
        .iter()
        .map(|entry| {
            entry
                .split_once('=')
                .map_or(entry.as_str(), |(key, _)| key)
                .to_lowercase()
        })
        .collect();
    assert!(folded.windows(2).all(|pair| pair[0] <= pair[1]));
}

#[test]
fn create_process_receives_exact_agent_overrides() {
    let output = env::temp_dir().join(format!("nmt-pty-env-{}.txt", process::id()));
    let _ = fs::remove_file(&output);
    let overrides = [
        ("NMT_AGENT_ROUTE".into(), "route-exact".into()),
        ("NMT_AGENT_HOOK_TOKEN".into(), "token-exact".into()),
        ("NMT_AGENT_HOOK_VERSION".into(), "1".into()),
    ];
    let mut environment = build_environment_block(&overrides);
    let command = format!(
        "cmd.exe /d /c (echo %NMT_AGENT_ROUTE%&echo %NMT_AGENT_HOOK_TOKEN%&echo %NMT_AGENT_HOOK_VERSION%)>\"{}\"",
        output.display()
    );
    let mut command: Vec<u16> = command.encode_utf16().chain([0]).collect();
    let mut startup: STARTUPINFOW = unsafe { mem::zeroed() };
    startup.cb = mem::size_of::<STARTUPINFOW>() as u32;
    let mut child: PROCESS_INFORMATION = unsafe { mem::zeroed() };
    let created = unsafe {
        CreateProcessW(
            ptr::null(),
            command.as_mut_ptr(),
            ptr::null_mut(),
            ptr::null_mut(),
            0,
            CREATE_UNICODE_ENVIRONMENT,
            environment.as_mut_ptr().cast(),
            ptr::null(),
            &startup,
            &mut child,
        )
    };
    assert_ne!(created, 0, "{}", io::Error::last_os_error());
    unsafe {
        WaitForSingleObject(child.hProcess, INFINITE);
        CloseHandle(child.hThread);
        CloseHandle(child.hProcess);
    }
    let values = fs::read_to_string(&output).unwrap();
    let _ = fs::remove_file(output);
    assert_eq!(
        values.lines().collect::<Vec<_>>(),
        ["route-exact", "token-exact", "1"]
    );
}
