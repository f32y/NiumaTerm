use std::path::Path;

use crate::windows::shell_integration::{
    context_menu_owned_registry_roots, context_menu_registered_registry_roots, dll_path_matches,
    protocol_command, shell_extension_path,
};

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
        Path::new(r"C:\Program Files\NiumaTerm\NmtShellExtension.dll")
    );
    assert!(dll_path_matches(
        r"c:\program files\niumaterm\NMTSHELLEXTENSION.DLL",
        Path::new(r"C:\Program Files\NiumaTerm\NmtShellExtension.dll")
    ));
    assert!(!dll_path_matches(
        r"C:\Old\NmtShellExtension.dll",
        Path::new(r"C:\Program Files\NiumaTerm\NmtShellExtension.dll")
    ));
}

#[test]
fn protocol_registration_quotes_executable_path() {
    assert_eq!(
        protocol_command(r"C:\Program Files\NiumaTerm\NiumaTerm.exe"),
        r#""C:\Program Files\NiumaTerm\NiumaTerm.exe" "%1""#
    );
}
