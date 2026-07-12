use std::path::Path;

use crate::NativeNotification;

#[cfg(target_os = "macos")]
mod platform {
    use std::sync::Once;

    use block2::RcBlock;
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};
    use objc2::runtime::Bool;
    use objc2_foundation::{NSError, NSString};
    use objc2_user_notifications::{
        UNAuthorizationOptions, UNMutableNotificationContent, UNNotificationRequest,
        UNUserNotificationCenter,
    };

    use super::NativeNotification;

    pub(crate) fn request_authorization() {
        static INIT: Once = Once::new();
        INIT.call_once(|| unsafe {
            let bundle: *mut Object = msg_send![class!(NSBundle), mainBundle];
            if bundle.is_null() {
                return;
            }
            let bundle_id: *mut Object = msg_send![bundle, bundleIdentifier];
            if bundle_id.is_null() {
                return;
            }

            let center = UNUserNotificationCenter::currentNotificationCenter();
            center.requestAuthorizationWithOptions_completionHandler(
                UNAuthorizationOptions::UNAuthorizationOptionAlert
                    | UNAuthorizationOptions::UNAuthorizationOptionSound,
                &RcBlock::new(|_ok: Bool, _err: *mut NSError| {}),
            );
        });
    }

    pub(crate) fn show(notification: &NativeNotification) -> Result<(), String> {
        unsafe {
            let bundle: *mut Object = msg_send![class!(NSBundle), mainBundle];
            if bundle.is_null() {
                return Ok(());
            }
            let bundle_id: *mut Object = msg_send![bundle, bundleIdentifier];
            if bundle_id.is_null() {
                return Ok(());
            }

            let center = UNUserNotificationCenter::currentNotificationCenter();
            let content = UNMutableNotificationContent::new();
            content.setTitle(&NSString::from_str(&notification.title));
            content.setBody(&NSString::from_str(&notification.body));
            let identifier = NSString::from_str("rio-notification");
            let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
                &identifier,
                &content,
                None,
            );
            center.addNotificationRequest_withCompletionHandler(&request, None);
        }
        Ok(())
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use std::collections::HashMap;

    use super::NativeNotification;

    pub(crate) fn show(notification: &NativeNotification) -> Result<(), String> {
        let Ok(connection) = zbus::blocking::Connection::session() else {
            return Ok(());
        };
        let Ok(proxy) = zbus::blocking::Proxy::new(
            &connection,
            "org.freedesktop.Notifications",
            "/org/freedesktop/Notifications",
            "org.freedesktop.Notifications",
        ) else {
            return Ok(());
        };
        let hints: HashMap<&str, zbus::zvariant::Value<'_>> = HashMap::new();
        let _: Result<u32, _> = proxy.call(
            "Notify",
            &(
                "Rio",
                0u32,
                "rio",
                &notification.title,
                &notification.body,
                &[] as &[&str],
                &hints,
                -1i32,
            ),
        );
        Ok(())
    }
}

#[cfg(target_os = "macos")]
pub(crate) use platform::request_authorization;
pub(crate) use platform::show;

pub(crate) fn remove(_tag: &str, _group: &str) -> Result<(), String> {
    Ok(())
}

pub(crate) fn register_identity(_exe_path: &Path) -> Result<(), String> {
    Ok(())
}

pub(crate) fn unregister_identity() -> Result<(), String> {
    Ok(())
}

pub(crate) fn identity_registered() -> bool {
    true
}
