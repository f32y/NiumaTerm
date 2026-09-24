// An in-process COM server that Explorer loads to put NiumaTerm on the shell
// context menu. Its exports are the entry points Explorer looks for in such a
// DLL, and nothing outside the Windows shell loads them, so off Windows the
// crate builds to an empty library. Dropping it from the workspace instead
// would take it out of the bare `cargo build` that produces the Windows
// release.
#![allow(non_snake_case)]
#![cfg(windows)]

mod server;

use std::ffi;

#[unsafe(no_mangle)]
extern "system" fn DllMain(
    instance: *mut ffi::c_void,
    reason: u32,
    _reserved: *mut ffi::c_void,
) -> bool {
    server::dll_main(instance, reason)
}

#[unsafe(no_mangle)]
unsafe extern "system" fn DllGetClassObject(
    rclsid: *const ffi::c_void,
    riid: *const ffi::c_void,
    output: *mut *mut ffi::c_void,
) -> i32 {
    unsafe { server::dll_get_class_object(rclsid, riid, output) }
}

#[unsafe(no_mangle)]
extern "system" fn DllCanUnloadNow() -> i32 {
    server::dll_can_unload_now()
}
