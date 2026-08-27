use std::path::{Path, PathBuf};
use std::{env, fs, io};

use windows::core::Error as WindowsError;

use crate::{APP_ID, NativeNotification};

fn shortcut_path() -> Result<PathBuf, String> {
    let app_data = env::var_os("APPDATA").ok_or("APPDATA is unavailable")?;
    Ok(PathBuf::from(app_data)
        .join("Microsoft")
        .join("Windows")
        .join("Start Menu")
        .join("Programs")
        .join("NiumaTerm.lnk"))
}

pub(crate) fn show(notification: &NativeNotification) -> Result<(), String> {
    use windows::Data::Xml::Dom::XmlDocument;
    use windows::UI::Notifications::{ToastNotification, ToastNotificationManager};
    use windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;
    use windows::core::HSTRING;

    unsafe { SetCurrentProcessExplicitAppUserModelID(&HSTRING::from(APP_ID)) }
        .map_err(|error| error.to_string())?;
    let xml = XmlDocument::new().map_err(|error| error.to_string())?;
    xml.LoadXml(&HSTRING::from(toast_xml(notification)))
        .map_err(|error| error.to_string())?;
    let toast =
        ToastNotification::CreateToastNotification(&xml).map_err(|error| error.to_string())?;
    if !notification.tag.is_empty() {
        toast
            .SetTag(&HSTRING::from(&notification.tag))
            .map_err(|error| error.to_string())?;
    }
    if !notification.group.is_empty() {
        toast
            .SetGroup(&HSTRING::from(&notification.group))
            .map_err(|error| error.to_string())?;
    }
    let notifier = ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(APP_ID))
        .map_err(|error| error.to_string())?;
    notifier.Show(&toast).map_err(|error| error.to_string())
}

pub(crate) fn remove(tag: &str, group: &str) -> Result<(), String> {
    use windows::UI::Notifications::ToastNotificationManager;
    use windows::core::HSTRING;

    ToastNotificationManager::History()
        .and_then(|history| {
            history.RemoveGroupedTagWithId(
                &HSTRING::from(tag),
                &HSTRING::from(group),
                &HSTRING::from(APP_ID),
            )
        })
        .map_err(|error| error.to_string())
}

pub(crate) fn register_identity(exe_path: &Path) -> Result<(), String> {
    use windows::Win32::Foundation::PROPERTYKEY;
    use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
        CoUninitialize, IPersistFile,
    };
    use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
    use windows::core::{Error, GUID, HSTRING, Interface};

    let shortcut = shortcut_path()?;
    fs::create_dir_all(shortcut.parent().expect("shortcut has parent"))
        .map_err(|error| error.to_string())?;
    let initialized = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    if initialized.is_err() {
        return Err(Error::from_hresult(initialized).to_string());
    }

    let result = (|| unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        let exe = HSTRING::from(exe_path.to_string_lossy().as_ref());
        link.SetPath(&exe)?;
        link.SetIconLocation(&exe, 0)?;

        const APP_ID_KEY: PROPERTYKEY = PROPERTYKEY {
            fmtid: GUID::from_u128(0x9f4c2855_9f79_4b39_a8d0_e1d42de1d5f3),
            pid: 5,
        };
        let store: IPropertyStore = link.cast()?;
        let app_id = PROPVARIANT::from(APP_ID);
        store.SetValue(&APP_ID_KEY, &app_id)?;
        store.Commit()?;

        let persist: IPersistFile = link.cast()?;
        persist.Save(&HSTRING::from(shortcut.to_string_lossy().as_ref()), true)
    })()
    .map_err(|error: WindowsError| error.to_string());
    unsafe { CoUninitialize() };
    result
}

pub(crate) fn unregister_identity() -> Result<(), String> {
    let shortcut = shortcut_path()?;
    match fs::remove_file(shortcut) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

pub(crate) fn identity_registered() -> bool {
    shortcut_path().is_ok_and(|path| path.is_file())
}

fn toast_xml(notification: &NativeNotification) -> String {
    let title = if notification.title.is_empty() {
        APP_ID
    } else {
        &notification.title
    };
    let activation = (!notification.activation_url.is_empty()).then(|| {
        format!(
            r#" activationType="protocol" launch="{}""#,
            escape_xml(&notification.activation_url, true)
        )
    });
    format!(
        r#"<toast{}><visual><binding template="ToastGeneric"><text>{}</text><text>{}</text></binding></visual></toast>"#,
        activation.unwrap_or_default(),
        escape_xml(title, false),
        escape_xml(&notification.body, false),
    )
}

fn escape_xml(value: &str, attribute: bool) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' if attribute => escaped.push_str("&quot;"),
            '\'' if attribute => escaped.push_str("&apos;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

#[cfg(test)]
mod tests;
