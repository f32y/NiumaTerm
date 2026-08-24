#![allow(non_snake_case)]

use std::ffi;

use nmt_platform::windows::shell_extension;

#[unsafe(no_mangle)]
extern "system" fn DllMain(
    instance: *mut ffi::c_void,
    reason: u32,
    _reserved: *mut ffi::c_void,
) -> bool {
    shell_extension::dll_main(instance, reason)
}

#[unsafe(no_mangle)]
unsafe extern "system" fn DllGetClassObject(
    rclsid: *const ffi::c_void,
    riid: *const ffi::c_void,
    output: *mut *mut ffi::c_void,
) -> i32 {
    unsafe { shell_extension::dll_get_class_object(rclsid, riid, output) }
}

#[unsafe(no_mangle)]
extern "system" fn DllCanUnloadNow() -> i32 {
    shell_extension::dll_can_unload_now()
}
