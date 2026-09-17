//! Application integration for the platform updater implementations.

#[cfg(target_os = "macos")]
pub(crate) use crate::update::macos::{can_check, check, initialize, on_settings_changed};
#[cfg(windows)]
pub(crate) use crate::update::notification::{UpdateNotification, install_error_text};
#[cfg(windows)]
pub(crate) use crate::update::windows::{
    cancel_install, check, close_file_users, complete_relaunch, continue_install, initialize,
    inspect_file_users, install_now, on_settings_changed, schedule_automatic_checks, status,
};

#[cfg(windows)]
mod file_users;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod notification;
#[cfg(windows)]
mod windows;

#[cfg(test)]
#[cfg(windows)]
mod tests;
