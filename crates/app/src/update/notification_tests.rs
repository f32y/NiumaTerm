use gpui::TestAppContext;
use gpui_component::Theme;
use nmt_updater::windows::InstallError;

use crate::update::notification::{UpdateNotice, UpdateNotification};

fn available(version: &str) -> UpdateNotice {
    UpdateNotice::Available {
        version: version.to_owned(),
        url: format!("https://github.com/f32y/NiumaTerm/releases/tag/{version}"),
    }
}

#[gpui::test]
fn dismissal_reaches_other_windows_and_survives_rechecks(cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_global(Theme::default()));

    let mut first = UpdateNotification::default();
    let mut second = UpdateNotification::default();

    let release = available("v1.1.0");
    let cx = cx.add_empty_window();

    let card = cx.update(|_, cx| {
        let card = first.sync(Some(release.clone()), cx).unwrap();

        assert!(second.sync(Some(release.clone()), cx).is_some());

        card
    });

    card.update_in(cx, |card, window, cx| card.dismiss(window, cx));

    cx.update(|_, cx| {
        assert!(first.sync(Some(release.clone()), cx).is_none());
        assert!(second.sync(Some(release.clone()), cx).is_none());
        assert!(second.sync(None, cx).is_none());
        assert!(second.sync(Some(release), cx).is_none());
        assert!(second.sync(Some(available("v1.2.0")), cx).is_some());
    });
}

#[gpui::test]
fn phase_changes_replace_the_card_in_place_and_clear_inactive_notices(cx: &mut TestAppContext) {
    let mut notification = UpdateNotification::default();

    cx.update(|cx| {
        let card = notification.sync(Some(available("v1.1.0")), cx).unwrap();

        let installing = notification
            .sync(Some(UpdateNotice::Installing("v1.1.0".into())), cx)
            .unwrap();

        let failed = notification
            .sync(Some(UpdateNotice::Failed(InstallError::Checksum)), cx)
            .unwrap();

        assert_eq!(card.entity_id(), installing.entity_id());
        assert_eq!(card.entity_id(), failed.entity_id());
        assert!(notification.sync(None, cx).is_none());
        assert!(notification.card.is_none());
    });
}

#[gpui::test]
fn hiding_progress_keeps_failures_visible_and_retries_can_report_the_same_error(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| cx.set_global(Theme::default()));

    let mut notification = UpdateNotification::default();

    let installing = UpdateNotice::Installing("v1.1.0".into());
    let failed = UpdateNotice::Failed(InstallError::Unreachable);
    let cx = cx.add_empty_window();
    let card = cx.update(|_, cx| notification.sync(Some(installing.clone()), cx).unwrap());

    card.update_in(cx, |card, window, cx| card.dismiss(window, cx));

    let error_card = cx.update(|_, cx| {
        assert!(notification.sync(Some(installing.clone()), cx).is_none());

        notification.sync(Some(failed.clone()), cx).unwrap()
    });

    error_card.update_in(cx, |card, window, cx| card.dismiss(window, cx));

    cx.update(|_, cx| {
        assert!(notification.sync(Some(failed.clone()), cx).is_none());
        assert!(notification.sync(Some(installing), cx).is_some());
        assert!(notification.sync(Some(failed), cx).is_some());
    });
}
