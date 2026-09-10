//! Native notification authorization, reported asynchronously by macOS.

use std::ptr::NonNull;
use std::sync::Arc;

use block2::RcBlock;
use objc2::runtime::Bool;
use objc2_foundation::{NSBundle, NSError};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNAuthorizationStatus, UNNotificationSettings, UNUserNotificationCenter,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotificationPermission {
    NotDetermined,
    Denied,
    Authorized,
    Provisional,
    Unavailable,
}

pub fn notification_permission(
    completion: impl Fn(NotificationPermission) + Send + Sync + 'static,
) {
    if NSBundle::mainBundle().bundleIdentifier().is_none() {
        completion(NotificationPermission::Unavailable);
        return;
    }
    UNUserNotificationCenter::currentNotificationCenter()
        .getNotificationSettingsWithCompletionHandler(&RcBlock::new(
            move |settings: NonNull<UNNotificationSettings>| {
                // The system retains the settings for the duration of this callback.
                let status = unsafe { settings.as_ref() }.authorizationStatus();
                let permission = match status {
                    UNAuthorizationStatus::NotDetermined => NotificationPermission::NotDetermined,
                    UNAuthorizationStatus::Denied => NotificationPermission::Denied,
                    UNAuthorizationStatus::Authorized => NotificationPermission::Authorized,
                    UNAuthorizationStatus::Provisional => NotificationPermission::Provisional,
                    _ => NotificationPermission::Unavailable,
                };
                completion(permission);
            },
        ));
}

pub fn request_notification_permission(
    completion: impl Fn(NotificationPermission) + Send + Sync + 'static,
) {
    if NSBundle::mainBundle().bundleIdentifier().is_none() {
        completion(NotificationPermission::Unavailable);
        return;
    }
    let completion = Arc::new(completion);
    UNUserNotificationCenter::currentNotificationCenter()
        .requestAuthorizationWithOptions_completionHandler(
            UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
            &RcBlock::new(move |_allowed: Bool, error: *mut NSError| {
                if !error.is_null() {
                    completion(NotificationPermission::Unavailable);
                    return;
                }
                let completion = completion.clone();
                notification_permission(move |status| completion(status));
            }),
        );
}
