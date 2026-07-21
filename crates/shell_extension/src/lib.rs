#![allow(non_snake_case)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use windows::Win32::Foundation::{
    CLASS_E_CLASSNOTAVAILABLE, E_NOINTERFACE, HINSTANCE, S_FALSE, S_OK,
};
use windows::Win32::System::Com::{CoTaskMemAlloc, IBindCtx, IClassFactory, IClassFactory_Impl};
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows::Win32::System::SystemServices::DLL_PROCESS_ATTACH;
use windows::Win32::UI::Shell::{
    IEnumExplorerCommand, IExplorerCommand, IExplorerCommand_Impl, IShellItemArray,
    SIGDN_FILESYSPATH,
};
use windows::core::{GUID, PWSTR, implement};
use windows_core::{BOOL, Interface, Ref};

const CLSID_NIUMATERM_NEW_TAB: GUID = GUID::from_u128(0xF1D94FEB_1AA5_4B27_9440_C3BC16247C61);

static mut DLL_INSTANCE: HINSTANCE = HINSTANCE(std::ptr::null_mut());
static DLL_REF_COUNT: AtomicU32 = AtomicU32::new(0);

#[unsafe(no_mangle)]
extern "system" fn DllMain(
    instance: HINSTANCE,
    reason: u32,
    _reserved: *mut std::ffi::c_void,
) -> bool {
    if reason == DLL_PROCESS_ATTACH {
        unsafe { DLL_INSTANCE = instance };
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
    let len = unsafe { GetModuleFileNameW(Some(DLL_INSTANCE.into()), &mut buf) };
    (len > 0).then(|| PathBuf::from(String::from_utf16_lossy(&buf[..len as usize])))
}

fn alloc_co_task_str(s: &str) -> PWSTR {
    let wide: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
    let byte_len = wide.len() * std::mem::size_of::<u16>();
    unsafe {
        let ptr = CoTaskMemAlloc(byte_len) as *mut u16;
        if !ptr.is_null() {
            std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr, wide.len());
        }
        PWSTR(ptr)
    }
}

fn get_folder_path(items: Option<&IShellItemArray>) -> Option<String> {
    let items = items?;
    unsafe {
        let item = items.GetItemAt(0).ok()?;
        let name = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let path = name.to_string().ok()?;
        windows::Win32::System::Com::CoTaskMemFree(Some(name.0 as *const _));
        Some(path)
    }
}

#[implement(IExplorerCommand)]
struct NiumaTermNewTabCommand;

impl IExplorerCommand_Impl for NiumaTermNewTabCommand_Impl {
    fn GetTitle(&self, _items: Ref<'_, IShellItemArray>) -> windows::core::Result<PWSTR> {
        Ok(alloc_co_task_str("Open in NiumaTerm"))
    }

    fn GetIcon(&self, _items: Ref<'_, IShellItemArray>) -> windows::core::Result<PWSTR> {
        let icon = format!("{},0", get_exe_path());
        Ok(alloc_co_task_str(&icon))
    }

    fn GetToolTip(&self, _items: Ref<'_, IShellItemArray>) -> windows::core::Result<PWSTR> {
        Ok(PWSTR::null())
    }

    fn GetCanonicalName(&self) -> windows::core::Result<GUID> {
        Ok(CLSID_NIUMATERM_NEW_TAB)
    }

    fn GetState(
        &self,
        _items: Ref<'_, IShellItemArray>,
        _ok_to_be_slow: BOOL,
    ) -> windows::core::Result<u32> {
        Ok(0x0) // ECS_ENABLED
    }

    fn Invoke(
        &self,
        items: Ref<'_, IShellItemArray>,
        _bind_ctx: Ref<'_, IBindCtx>,
    ) -> windows::core::Result<()> {
        let exe = get_exe_path();
        let path = get_folder_path(items.as_ref()).unwrap_or_default();
        let uri = format!(
            "nmt://action/new_tab?path={}",
            utf8_percent_encode(&path, NON_ALPHANUMERIC)
        );
        let _ = std::process::Command::new(&exe).arg(&uri).spawn();
        Ok(())
    }

    fn GetFlags(&self) -> windows::core::Result<u32> {
        Ok(0) // ECF_DEFAULT
    }

    fn EnumSubCommands(&self) -> windows::core::Result<IEnumExplorerCommand> {
        Err(windows::core::Error::from(E_NOINTERFACE))
    }
}

#[implement(IClassFactory)]
struct NiumaTermClassFactory;

impl IClassFactory_Impl for NiumaTermClassFactory_Impl {
    fn CreateInstance(
        &self,
        punkouter: Ref<'_, windows::core::IUnknown>,
        riid: *const GUID,
        ppvobject: *mut *mut std::ffi::c_void,
    ) -> windows::core::Result<()> {
        if ppvobject.is_null() || riid.is_null() {
            return Err(windows::Win32::Foundation::E_POINTER.into());
        }

        if !punkouter.is_null() {
            return Err(windows::core::Error::from(
                windows::Win32::Foundation::CLASS_E_NOAGGREGATION,
            ));
        }

        unsafe {
            *ppvobject = std::ptr::null_mut();

            let cmd: IExplorerCommand = NiumaTermNewTabCommand.into();
            let obj: windows::core::IUnknown = cmd.cast()?;

            obj.query(&*riid, ppvobject).ok()
        }
    }

    fn LockServer(&self, flock: BOOL) -> windows::core::Result<()> {
        if flock.as_bool() {
            DLL_REF_COUNT.fetch_add(1, Ordering::Relaxed);
        } else {
            DLL_REF_COUNT.fetch_sub(1, Ordering::Relaxed);
        }
        Ok(())
    }
}

#[unsafe(no_mangle)]
unsafe extern "system" fn DllGetClassObject(
    rclsid: *const GUID,
    riid: *const GUID,
    ppv: *mut *mut std::ffi::c_void,
) -> windows::core::HRESULT {
    if rclsid.is_null() || riid.is_null() || ppv.is_null() {
        return E_NOINTERFACE;
    }

    unsafe {
        *ppv = std::ptr::null_mut();

        let clsid = *rclsid;
        if clsid != CLSID_NIUMATERM_NEW_TAB {
            return CLASS_E_CLASSNOTAVAILABLE;
        }

        let factory = NiumaTermClassFactory;
        let factory: IClassFactory = factory.into();

        factory.query(&*riid, ppv)
    }
}

#[unsafe(no_mangle)]
extern "system" fn DllCanUnloadNow() -> windows::core::HRESULT {
    if DLL_REF_COUNT.load(Ordering::Relaxed) == 0 {
        S_OK
    } else {
        S_FALSE
    }
}
