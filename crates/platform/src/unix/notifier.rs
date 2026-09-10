use crate::NativeNotification;

#[cfg(target_os = "macos")]
mod platform {
    use std::sync::Once;

    use block2::RcBlock;
    use objc2::runtime::Bool;
    use objc2_foundation::{NSBundle, NSError, NSString};
    use objc2_user_notifications::{
        UNAuthorizationOptions, UNMutableNotificationContent, UNNotificationRequest,
        UNUserNotificationCenter,
    };

    use super::NativeNotification;

    /// User notifications are delivered on behalf of a bundle identifier, so a
    /// binary run outside an application bundle has nothing to post them as and
    /// asking would raise rather than return an error.
    fn is_bundled() -> bool {
        NSBundle::mainBundle().bundleIdentifier().is_some()
    }

    pub(crate) fn request_authorization() {
        if !is_bundled() {
            return;
        }

        static INIT: Once = Once::new();
        INIT.call_once(|| {
            let center = UNUserNotificationCenter::currentNotificationCenter();

            center.requestAuthorizationWithOptions_completionHandler(
                UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
                &RcBlock::new(|_ok: Bool, _err: *mut NSError| {}),
            );
        });
    }

    pub(crate) fn show(notification: &NativeNotification) -> Result<(), String> {
        if !is_bundled() {
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

        Ok(())
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use std::collections::HashMap;

    use zbus::blocking::{Connection, Proxy};
    use zbus::zvariant::Value;

    use super::NativeNotification;

    pub(crate) fn show(notification: &NativeNotification) -> Result<(), String> {
        let Ok(connection) = Connection::session() else {
            return Ok(());
        };

        let Ok(proxy) = Proxy::new(
            &connection,
            "org.freedesktop.Notifications",
            "/org/freedesktop/Notifications",
            "org.freedesktop.Notifications",
        ) else {
            return Ok(());
        };

        let hints: HashMap<&str, Value<'_>> = HashMap::new();

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
