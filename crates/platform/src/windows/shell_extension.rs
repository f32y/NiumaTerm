#![allow(non_snake_case)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicPtr, AtomicU32, Ordering};
use std::{ffi, iter, mem, process, ptr};

use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use windows::Win32::Foundation::{
    CLASS_E_CLASSNOTAVAILABLE, CLASS_E_NOAGGREGATION, E_NOINTERFACE, E_POINTER, HINSTANCE, S_FALSE,
    S_OK,
};
use windows::Win32::System::Com::{
    CoTaskMemAlloc, CoTaskMemFree, IBindCtx, IClassFactory, IClassFactory_Impl,
};
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows::Win32::System::SystemServices::DLL_PROCESS_ATTACH;
use windows::Win32::UI::Shell::{
    IEnumExplorerCommand, IExplorerCommand, IExplorerCommand_Impl, IShellItemArray,
    SIGDN_FILESYSPATH,
};
use windows::core::{Error, GUID, IUnknown, PWSTR, Result, implement};
use windows_core::{BOOL, Interface, Ref};

const CLSID_NIUMATERM_NEW_TAB: GUID = GUID::from_u128(0xF1D94FEB_1AA5_4B27_9440_C3BC16247C61);

// AtomicPtr instead of `static mut`: written from DllMain, read from COM
// calls on arbitrary threads; the atomic removes the unsynchronized
// shared-mutable-static access without changing behavior.
static DLL_INSTANCE: AtomicPtr<ffi::c_void> = AtomicPtr::new(ptr::null_mut());
/// Combined LockServer count + live COM object count (the classic ATL
/// module count). DllCanUnloadNow must stay S_FALSE while any command or
/// factory object is alive, or Explorer can unload the DLL under an object
/// whose vtable still points into it.
static DLL_REF_COUNT: AtomicU32 = AtomicU32::new(0);

fn dll_add_ref() {
    DLL_REF_COUNT.fetch_add(1, Ordering::Relaxed);
}

fn dll_release() {
    DLL_REF_COUNT.fetch_sub(1, Ordering::Relaxed);
}

pub fn dll_main(instance: *mut ffi::c_void, reason: u32) -> bool {
    if reason == DLL_PROCESS_ATTACH {
        DLL_INSTANCE.store(instance, Ordering::Relaxed);
    }
    true
}

fn get_exe_path() -> String {
    dll_path()
        .and_then(|path| path.parent().map(|dir| dir.join("NiumaTerm.exe")))
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| "NiumaTerm.exe".to_string())
}

fn dll_path() -> Option<PathBuf> {
    let mut buf = [0u16; 32768];

    let instance = HINSTANCE(DLL_INSTANCE.load(Ordering::Relaxed));

    let len = unsafe { GetModuleFileNameW(Some(instance.into()), &mut buf) };

    (len > 0).then(|| PathBuf::from(String::from_utf16_lossy(&buf[..len as usize])))
}

fn alloc_co_task_str(s: &str) -> PWSTR {
    let wide: Vec<u16> = s.encode_utf16().chain(iter::once(0)).collect();
    let byte_len = wide.len() * mem::size_of::<u16>();

    unsafe {
        let ptr = CoTaskMemAlloc(byte_len) as *mut u16;

        if !ptr.is_null() {
            ptr::copy_nonoverlapping(wide.as_ptr(), ptr, wide.len());
        }

        PWSTR(ptr)
    }
}

fn get_folder_path(items: Option<&IShellItemArray>) -> Option<String> {
    let items = items?;

    unsafe {
        let item = items.GetItemAt(0).ok()?;

        let name = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;

        // Convert before freeing, free before the ?-return: bailing out on a
        // failed UTF-16 conversion must not leak the CoTaskMem string.
        let path = name.to_string();

        CoTaskMemFree(Some(name.0 as *const _));

        path.ok()
    }
}

#[implement(IExplorerCommand)]
struct NiumaTermNewTabCommand;

impl NiumaTermNewTabCommand {
    fn new() -> Self {
        dll_add_ref();
        Self
    }
}

impl Drop for NiumaTermNewTabCommand {
    fn drop(&mut self) {
        dll_release();
    }
}

impl IExplorerCommand_Impl for NiumaTermNewTabCommand_Impl {
    fn GetTitle(&self, _items: Ref<'_, IShellItemArray>) -> Result<PWSTR> {
        Ok(alloc_co_task_str("Open in NiumaTerm"))
    }

    fn GetIcon(&self, _items: Ref<'_, IShellItemArray>) -> Result<PWSTR> {
        let icon = format!("{},0", get_exe_path());
        Ok(alloc_co_task_str(&icon))
    }

    fn GetToolTip(&self, _items: Ref<'_, IShellItemArray>) -> Result<PWSTR> {
        Ok(PWSTR::null())
    }

    fn GetCanonicalName(&self) -> Result<GUID> {
        Ok(CLSID_NIUMATERM_NEW_TAB)
    }

    fn GetState(&self, _items: Ref<'_, IShellItemArray>, _ok_to_be_slow: BOOL) -> Result<u32> {
        Ok(0x0) // ECS_ENABLED
    }

    fn Invoke(&self, items: Ref<'_, IShellItemArray>, _bind_ctx: Ref<'_, IBindCtx>) -> Result<()> {
        let exe = get_exe_path();

        let path = get_folder_path(items.as_ref()).unwrap_or_default();

        let uri = format!(
            "nmt://action/new_tab?path={}",
            utf8_percent_encode(&path, NON_ALPHANUMERIC)
        );

        let _ = process::Command::new(&exe).arg(&uri).spawn();

        Ok(())
    }

    fn GetFlags(&self) -> Result<u32> {
        Ok(0) // ECF_DEFAULT
    }

    fn EnumSubCommands(&self) -> Result<IEnumExplorerCommand> {
        Err(Error::from(E_NOINTERFACE))
    }
}

#[implement(IClassFactory)]
struct NiumaTermClassFactory;

impl NiumaTermClassFactory {
    fn new() -> Self {
        dll_add_ref();
        Self
    }
}

impl Drop for NiumaTermClassFactory {
    fn drop(&mut self) {
        dll_release();
    }
}

impl IClassFactory_Impl for NiumaTermClassFactory_Impl {
    fn CreateInstance(
        &self,
        punkouter: Ref<'_, IUnknown>,
        riid: *const GUID,
        ppvobject: *mut *mut ffi::c_void,
    ) -> Result<()> {
        if ppvobject.is_null() || riid.is_null() {
            return Err(E_POINTER.into());
        }

        if !punkouter.is_null() {
            return Err(Error::from(CLASS_E_NOAGGREGATION));
        }

        unsafe {
            *ppvobject = ptr::null_mut();

            let cmd: IExplorerCommand = NiumaTermNewTabCommand::new().into();
            let obj: IUnknown = cmd.cast()?;

            obj.query(&*riid, ppvobject).ok()
        }
    }

    fn LockServer(&self, flock: BOOL) -> Result<()> {
        if flock.as_bool() {
            DLL_REF_COUNT.fetch_add(1, Ordering::Relaxed);
        } else {
            // Saturating decrement: one unbalanced unlock must not wrap the
            // count to u32::MAX and pin the DLL in memory forever.
            let _ = DLL_REF_COUNT.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                count.checked_sub(1)
            });
        }

        Ok(())
    }
}

pub unsafe fn dll_get_class_object(
    rclsid: *const ffi::c_void,
    riid: *const ffi::c_void,
    ppv: *mut *mut ffi::c_void,
) -> i32 {
    if rclsid.is_null() || riid.is_null() || ppv.is_null() {
        return E_NOINTERFACE.0;
    }

    unsafe {
        *ppv = ptr::null_mut();

        let clsid = *rclsid.cast::<GUID>();
        if clsid != CLSID_NIUMATERM_NEW_TAB {
            return CLASS_E_CLASSNOTAVAILABLE.0;
        }

        let factory: IClassFactory = NiumaTermClassFactory::new().into();

        factory.query(&*riid.cast::<GUID>(), ppv).0
    }
}

pub fn dll_can_unload_now() -> i32 {
    if DLL_REF_COUNT.load(Ordering::Relaxed) == 0 {
        S_OK.0
    } else {
        S_FALSE.0
    }
}
