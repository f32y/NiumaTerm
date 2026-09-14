pub(crate) use crate::unix::notifier::platform::{remove, show};

#[cfg(target_os = "macos")]
mod platform {
    use std::collections::HashMap;
    use std::process;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Arc, OnceLock};

    use block2::RcBlock;
    use objc2::runtime::Bool;
    use objc2_foundation::{NSArray, NSBundle, NSError, NSString};
    use objc2_user_notifications::{
        UNAuthorizationOptions, UNMutableNotificationContent, UNNotificationRequest,
        UNUserNotificationCenter,
    };
    use parking_lot::Mutex;
    use tracing::warn;

    use crate::NativeNotification;

    type RequestMap = HashMap<(String, String), Arc<RequestState>>;

    static REQUESTS: OnceLock<Mutex<RequestMap>> = OnceLock::new();

    static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

    struct RequestState {
        identifier: String,
        cancelled: AtomicBool,
    }

    impl RequestState {
        fn cancel(&self) {
            self.cancelled.store(true, Ordering::Release);

            remove_native(&self.identifier);
        }
    }

    pub(crate) fn show(notification: &NativeNotification) -> Result<(), String> {
        if NSBundle::mainBundle().bundleIdentifier().is_none() {
            return Err("native notifications require an application bundle".into());
        }

        let generation = NEXT_GENERATION.fetch_add(1, Ordering::Relaxed);

        let state = Arc::new(RequestState {
            // Each submission needs a different identifier: an older add
            // completion can still remove its own cancelled notification.
            identifier: format!(
                "{}:{}:{}:{generation}",
                notification.group,
                notification.tag,
                process::id(),
            ),
            cancelled: AtomicBool::new(false),
        });

        let previous = REQUESTS.get_or_init(Mutex::default).lock().insert(
            (notification.group.clone(), notification.tag.clone()),
            Arc::clone(&state),
        );

        if let Some(previous) = previous {
            previous.cancel();
        }

        let center = UNUserNotificationCenter::currentNotificationCenter();
        let content = UNMutableNotificationContent::new();

        content.setTitle(&NSString::from_str(&notification.title));

        content.setBody(&NSString::from_str(&notification.body));

        let identifier = NSString::from_str(&state.identifier);

        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
            &identifier,
            &content,
            None,
        );

        // Permission is asynchronous; submitting before its completion loses
        // the first notification while the user is answering the prompt.
        center.requestAuthorizationWithOptions_completionHandler(
            UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
            &RcBlock::new(move |granted: Bool, error: *mut NSError| {
                // SAFETY: the framework keeps NSError alive until this callback returns.
                if let Some(error) = unsafe { error.as_ref() } {
                    warn!(
                        "notification authorization failed: {}",
                        error.localizedDescription()
                    );

                    return;
                }

                if !granted.as_bool() {
                    warn!("notification authorization was denied");

                    return;
                }

                if state.cancelled.load(Ordering::Acquire) {
                    return;
                }

                let submitted = Arc::clone(&state);

                let completion = RcBlock::new(move |error: *mut NSError| {
                    // SAFETY: the framework keeps NSError alive until this callback returns.
                    if let Some(error) = unsafe { error.as_ref() } {
                        warn!(
                            "native notification delivery failed: {}",
                            error.localizedDescription()
                        );
                    }

                    // Cancellation can race between the authorization check
                    // and native submission. Repeat removal after add finishes.
                    if submitted.cancelled.load(Ordering::Acquire) {
                        remove_native(&submitted.identifier);
                    }
                });

                UNUserNotificationCenter::currentNotificationCenter()
                    .addNotificationRequest_withCompletionHandler(&request, Some(&completion));
            }),
        );

        Ok(())
    }

    pub(crate) fn remove(tag: &str, group: &str) -> Result<(), String> {
        if NSBundle::mainBundle().bundleIdentifier().is_none() {
            return Err("native notifications require an application bundle".into());
        }

        let state = REQUESTS
            .get_or_init(Mutex::default)
            .lock()
            .remove(&(group.to_string(), tag.to_string()));

        if let Some(state) = state {
            state.cancel();
        }

        Ok(())
    }

    fn remove_native(identifier: &str) {
        let identifier = NSString::from_str(identifier);
        let identifiers = NSArray::from_slice(&[&*identifier]);
        let center = UNUserNotificationCenter::currentNotificationCenter();

        center.removePendingNotificationRequestsWithIdentifiers(&identifiers);

        center.removeDeliveredNotificationsWithIdentifiers(&identifiers);
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use std::collections::HashMap;
    use std::sync::OnceLock;

    use parking_lot::Mutex;
    use zbus::blocking::{Connection, Proxy};
    use zbus::zvariant::Value;

    use crate::NativeNotification;

    type NotificationIds = HashMap<(String, String), u32>;

    static IDS: OnceLock<Mutex<NotificationIds>> = OnceLock::new();

    pub(crate) fn show(notification: &NativeNotification) -> Result<(), String> {
        let connection = Connection::session().map_err(|error| error.to_string())?;

        let proxy = Proxy::new(
            &connection,
            "org.freedesktop.Notifications",
            "/org/freedesktop/Notifications",
            "org.freedesktop.Notifications",
        )
        .map_err(|error| error.to_string())?;

        let hints: HashMap<&str, Value<'_>> = HashMap::new();
        let key = (notification.group.clone(), notification.tag.clone());

        let mut ids = IDS.get_or_init(Mutex::default).lock();

        let previous = ids.get(&key).copied().unwrap_or(0);

        let id: u32 = proxy
            .call(
                "Notify",
                &(
                    "NiumaTerm",
                    previous,
                    "NiumaTerm",
                    &notification.title,
                    &notification.body,
                    &[] as &[&str],
                    &hints,
                    -1i32,
                ),
            )
            .map_err(|error| error.to_string())?;

        ids.insert(key, id);

        Ok(())
    }

    pub(crate) fn remove(tag: &str, group: &str) -> Result<(), String> {
        let mut ids = IDS.get_or_init(Mutex::default).lock();

        let key = (group.to_string(), tag.to_string());

        let Some(id) = ids.get(&key).copied() else {
            return Ok(());
        };

        let connection = Connection::session().map_err(|error| error.to_string())?;

        let proxy = Proxy::new(
            &connection,
            "org.freedesktop.Notifications",
            "/org/freedesktop/Notifications",
            "org.freedesktop.Notifications",
        )
        .map_err(|error| error.to_string())?;

        let (): () = proxy
            .call("CloseNotification", &id)
            .map_err(|error| error.to_string())?;

        ids.remove(&key);

        Ok(())
    }
}
