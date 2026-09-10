use std::io::ErrorKind;
use std::path::Path;
use std::{env, mem, process};

use crate::library::{LibrarySymbol, ResidentLibrary};

#[test]
fn missing_library_and_embedded_nul_return_errors() {
    let missing = env::temp_dir()
        .join(format!("nmt-missing-library-{}", process::id()))
        .join("missing-library");
    // Neither path can identify executable code to initialize.
    assert!(unsafe { ResidentLibrary::load(&missing) }.is_err());
    let error = unsafe { ResidentLibrary::load(Path::new("invalid\0library")) }
        .err()
        .expect("embedded NUL must not truncate the path");
    assert_eq!(error.kind(), ErrorKind::InvalidInput);
}

#[cfg(any(
    windows,
    all(target_os = "linux", target_env = "gnu"),
    target_os = "macos"
))]
#[test]
fn system_library_exports_resolve_and_missing_exports_are_absent() {
    let address = {
        #[cfg(windows)]
        let path = Path::new(&env::var_os("SystemRoot").unwrap())
            .join("System32")
            .join("kernel32.dll");
        #[cfg(target_os = "linux")]
        let path = Path::new("libc.so.6");
        #[cfg(target_os = "macos")]
        let path = Path::new("/usr/lib/libSystem.B.dylib");
        // The operating system's standard process library is trusted code.
        let library = unsafe { ResidentLibrary::load(path.as_ref()) }.unwrap();
        assert!(library.symbol(c"nmt_missing_export_9381").is_none());
        #[cfg(windows)]
        let name = c"GetCurrentProcessId";
        #[cfg(unix)]
        let name = c"getpid";
        library.symbol(name).unwrap()
    };
    #[cfg(windows)]
    type ProcessId = unsafe extern "system" fn() -> u32;
    #[cfg(unix)]
    type ProcessId = unsafe extern "C" fn() -> libc::pid_t;
    // The named system export takes no arguments and returns the process ID.
    let process_id = unsafe { mem::transmute::<LibrarySymbol, ProcessId>(address) };
    assert_eq!(unsafe { process_id() } as u32, process::id());
}

#[test]
#[ignore = "requires NMT_TEST_SYNTAX_BUNDLE pointing to the built parser library"]
fn parser_export_remains_callable_after_load_scope() {
    let path = env::var_os("NMT_TEST_SYNTAX_BUNDLE").expect("parser library path");
    let address = {
        // The fixture is the parser library built from this workspace.
        let library = unsafe { ResidentLibrary::load(Path::new(&path)) }.unwrap();
        library.symbol(c"nmt_tree_sitter_abi_version").unwrap()
    };
    type AbiVersion = unsafe extern "system" fn() -> u32;
    // The bundle exports this signature and remains mapped after registration.
    let abi_version = unsafe { mem::transmute::<LibrarySymbol, AbiVersion>(address) };
    assert_eq!(unsafe { abi_version() }, 1);
}
