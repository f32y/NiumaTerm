use std::path::{Path, PathBuf};

use anyhow::Result;
use windows_registry::CURRENT_USER;

use super::notifier;

const CLSID_NEW_TAB: &str = "{f1d94feb-1aa5-4b27-9440-c3bc16247c61}";

const VERBS: [Verb; 1] = [Verb {
    id: "NiumaTermNewTab",
    title: "Open in NiumaTerm",
    clsid: CLSID_NEW_TAB,
}];

const LEGACY_NEW_WINDOW_ROOTS: [&str; 3] = [
    r"Software\Classes\CLSID\{f240799c-a056-4f34-a6b0-926b9730ce3f}",
    r"Software\Classes\Directory\shell\NiumaTermNewWindow",
    r"Software\Classes\Directory\Background\shell\NiumaTermNewWindow",
];

const ITEM_TYPES: [&str; 2] = [r"Directory", r"Directory\Background"];
const NMT_PROTOCOL_ROOT: &str = r"Software\Classes\nmt";

struct Verb {
    id: &'static str,
    title: &'static str,
    clsid: &'static str,
}

pub fn register_shell_integration() -> Result<()> {
    let exe_path = std::env::current_exe()?;
    let dll_path = shell_extension_path(&exe_path);
    register_shell_integration_paths(&exe_path, &dll_path)
}

pub fn unregister_shell_integration() -> Result<()> {
    for path in context_menu_owned_registry_roots() {
        let _ = CURRENT_USER.remove_tree(path);
    }
    Ok(())
}

pub fn is_shell_integration_registered() -> bool {
    context_menu_registered_registry_roots()
        .into_iter()
        .all(|path| CURRENT_USER.open(path).is_ok())
}

pub fn shell_integration_dll_mismatched() -> bool {
    if !context_menu_owned_registry_roots()
        .into_iter()
        .any(|path| CURRENT_USER.open(path).is_ok())
    {
        return false;
    }

    let Ok(exe_path) = std::env::current_exe() else {
        return true;
    };
    let expected = shell_extension_path(&exe_path);

    VERBS.iter().any(|verb| {
        CURRENT_USER
            .open(format!(
                r"Software\Classes\CLSID\{}\InprocServer32",
                verb.clsid
            ))
            .and_then(|key| key.get_string(""))
            .map_or(true, |actual| !dll_path_matches(&actual, &expected))
    })
}

pub fn set_system_notification_enabled(enabled: bool) -> Result<()> {
    if enabled {
        let exe_path = std::env::current_exe()?;
        let exe_path = path_string(&exe_path);
        let icon = format!("{exe_path},0");
        let protocol = CURRENT_USER.create(NMT_PROTOCOL_ROOT)?;
        protocol.set_string("", "URL:NiumaTerm Protocol")?;
        protocol.set_string("URL Protocol", "")?;
        protocol.create("DefaultIcon")?.set_string("", &icon)?;
        protocol
            .create(r"shell\open\command")?
            .set_string("", protocol_command(&exe_path))?;
        notifier::register_identity(Path::new(&exe_path)).map_err(anyhow::Error::msg)
    } else {
        let _ = CURRENT_USER.remove_tree(NMT_PROTOCOL_ROOT);
        notifier::unregister_identity().map_err(anyhow::Error::msg)
    }
}

pub fn system_notification_enabled() -> bool {
    CURRENT_USER
        .open(format!(r"{NMT_PROTOCOL_ROOT}\shell\open\command"))
        .is_ok()
        && notifier::identity_registered()
}

pub(crate) fn register_shell_integration_paths(exe_path: &Path, dll_path: &Path) -> Result<()> {
    let exe_path = path_string(exe_path);
    let dll_path = path_string(dll_path);
    let icon = format!("{exe_path},0");

    for path in LEGACY_NEW_WINDOW_ROOTS {
        let _ = CURRENT_USER.remove_tree(path);
    }

    for verb in VERBS {
        let clsid = CURRENT_USER.create(format!(r"Software\Classes\CLSID\{}", verb.clsid))?;
        clsid.set_string("", verb.title)?;
        let inproc = clsid.create("InprocServer32")?;
        inproc.set_string("", &dll_path)?;
        inproc.set_string("ThreadingModel", "Apartment")?;

        for item_type in ITEM_TYPES {
            let path = format!(r"Software\Classes\{item_type}\shell\{}", verb.id);
            let key = CURRENT_USER.create(path)?;
            key.set_string("MUIVerb", verb.title)?;
            key.set_string("Icon", &icon)?;
            key.set_string("ExplorerCommandHandler", verb.clsid)?;
        }
    }

    Ok(())
}

fn shell_extension_path(exe_path: &Path) -> PathBuf {
    exe_path.with_file_name("shell_extension.dll")
}

fn dll_path_matches(actual: &str, expected: &Path) -> bool {
    actual.eq_ignore_ascii_case(&path_string(expected))
}

fn protocol_command(exe_path: &str) -> String {
    format!(r#""{exe_path}" "%1""#)
}

fn context_menu_owned_registry_roots() -> Vec<String> {
    let mut roots = Vec::new();
    for verb in VERBS {
        roots.push(format!(r"Software\Classes\CLSID\{}", verb.clsid));
        for item_type in ITEM_TYPES {
            roots.push(format!(r"Software\Classes\{item_type}\shell\{}", verb.id));
        }
    }
    roots.extend(LEGACY_NEW_WINDOW_ROOTS.map(String::from));
    roots
}

fn context_menu_registered_registry_roots() -> Vec<String> {
    let mut roots = Vec::new();
    for verb in VERBS {
        roots.push(format!(
            r"Software\Classes\CLSID\{}\InprocServer32",
            verb.clsid
        ));
        for item_type in ITEM_TYPES {
            roots.push(format!(r"Software\Classes\{item_type}\shell\{}", verb.id));
        }
    }
    roots
}

fn path_string(path: &Path) -> String {
    path.as_os_str().to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unregister_only_removes_owned_roots() {
        assert_eq!(
            context_menu_owned_registry_roots(),
            [
                r"Software\Classes\CLSID\{f1d94feb-1aa5-4b27-9440-c3bc16247c61}",
                r"Software\Classes\Directory\shell\NiumaTermNewTab",
                r"Software\Classes\Directory\Background\shell\NiumaTermNewTab",
                r"Software\Classes\CLSID\{f240799c-a056-4f34-a6b0-926b9730ce3f}",
                r"Software\Classes\Directory\shell\NiumaTermNewWindow",
                r"Software\Classes\Directory\Background\shell\NiumaTermNewWindow",
            ]
        );
    }

    #[test]
    fn registered_check_uses_com_roots() {
        assert_eq!(
            context_menu_registered_registry_roots(),
            [
                r"Software\Classes\CLSID\{f1d94feb-1aa5-4b27-9440-c3bc16247c61}\InprocServer32",
                r"Software\Classes\Directory\shell\NiumaTermNewTab",
                r"Software\Classes\Directory\Background\shell\NiumaTermNewTab",
            ]
        );
        assert_eq!(
            shell_extension_path(Path::new(r"C:\Program Files\NiumaTerm\NiumaTerm.exe")),
            Path::new(r"C:\Program Files\NiumaTerm\shell_extension.dll")
        );
        assert!(dll_path_matches(
            r"c:\program files\niumaterm\SHELL_EXTENSION.DLL",
            Path::new(r"C:\Program Files\NiumaTerm\shell_extension.dll")
        ));
        assert!(!dll_path_matches(
            r"C:\Old\shell_extension.dll",
            Path::new(r"C:\Program Files\NiumaTerm\shell_extension.dll")
        ));
    }

    #[test]
    fn protocol_registration_quotes_executable_path() {
        assert_eq!(
            protocol_command(r"C:\Program Files\NiumaTerm\NiumaTerm.exe"),
            r#""C:\Program Files\NiumaTerm\NiumaTerm.exe" "%1""#
        );
    }
}
