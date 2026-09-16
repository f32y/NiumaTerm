//! Retains Sparkle for the application's lifetime and connects settings/menu
//! actions to the updater. Native update windows are owned by Sparkle.

use gpui::{App, Global};
use nmt_updater::macos::{StartError, Updater};
use tracing::{info, warn};

use crate::ui::AppSettings;

struct AppUpdate(Updater);

impl Global for AppUpdate {}

pub(crate) fn initialize(testing: bool, cx: &mut App) {
    let settings = &cx.global::<AppSettings>().config().update;

    match Updater::start(settings, testing) {
        Ok(Some(updater)) => cx.set_global(AppUpdate(updater)),
        Ok(None) => {}
        Err(StartError::NoFeedConfigured) => {
            info!("this build has no update feed; automatic updates are off");
        }
        Err(error) => warn!("the updater did not start: {error}"),
    }
}

pub(crate) fn on_settings_changed(cx: &mut App) {
    if let Some(update) = cx.try_global::<AppUpdate>() {
        update
            .0
            .set_settings(&cx.global::<AppSettings>().config().update);
    }
}

pub(crate) fn check(cx: &App) {
    if let Some(update) = cx.try_global::<AppUpdate>() {
        update.0.check();
    }
}

pub(crate) fn can_check(cx: &App) -> bool {
    cx.try_global::<AppUpdate>()
        .is_some_and(|update| update.0.can_check())
}
