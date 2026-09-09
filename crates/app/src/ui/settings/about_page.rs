#[cfg(any(windows, target_os = "macos"))]
use nmt_config::update::UpdateChannel;
use nmt_i18n::i18n;

#[cfg(target_os = "macos")]
use crate::sparkle;
use crate::ui::settings::*;
#[cfg(windows)]
use crate::update::{self, CheckError, InstallError, Status};

pub(super) fn about_page() -> SettingPage {
    // One untitled group. A page holding a single group drops its subcategory
    // entries from the sidebar and stops appending a group name to the page
    // header, so what the build is and how it updates read as one page instead
    // of two the user switches between.
    let group = SettingGroup::new()
        .item(SettingItem::new(
            i18n("settings-about-version"),
            SettingField::render(|_, _, _| Label::new(APP_VERSION).text_sm()),
        ))
        .item(SettingItem::new(
            i18n("settings-about-internal-version"),
            SettingField::render(|_, _, _| Label::new(APP_INTERNAL_VERSION).text_sm()),
        ))
        .item(SettingItem::new(
            i18n("settings-about-releases"),
            SettingField::render(|_, _, _| {
                Button::new("go-to-release-page")
                    .outline()
                    .label(i18n("settings-about-release-page"))
                    .on_click(|_, _, cx: &mut App| cx.open_url(RELEASE_PAGE_URL))
            }),
        ));

    // Everything that follows drives an updater, and only some builds have one.
    // The release page above is what remains: a build that cannot replace
    // itself can still be replaced by hand.
    #[cfg(any(windows, target_os = "macos"))]
    let group = group
        .item(
            SettingItem::new(
                i18n("settings-about-check-updates"),
                SettingField::switch(
                    |cx| cx.global::<AppSettings>().check_updates,
                    |value, cx| {
                        cx.global_mut::<AppSettings>().check_updates = value;
                    },
                ),
            )
            .description(i18n("settings-about-check-updates-description")),
        )
        .item(SettingItem::new(
            i18n("settings-about-channel"),
            SettingField::dropdown(
                vec![
                    (
                        "stable".into(),
                        i18n("settings-about-channel-stable").into(),
                    ),
                    (
                        "nightly".into(),
                        i18n("settings-about-channel-nightly").into(),
                    ),
                ],
                |cx| cx.global::<AppSettings>().update_channel.as_str().into(),
                |value, cx| {
                    cx.global_mut::<AppSettings>().update_channel =
                        UpdateChannel::from_value(&value);
                },
            )
            .default_value(SharedString::from("stable")),
        ))
        .item(update_check_item());

    SettingPage::new(i18n("settings-about-title"))
        .default_open(true)
        .group(group)
}

/// Sparkle owns the whole state machine of a check and draws its own progress
/// and result windows, so there is no status for this row to report the way the
/// Windows one does. What is left is the button, and whether it can be pressed:
/// `can_check` is false while a check is already running and for a build with no
/// updater at all.
#[cfg(target_os = "macos")]
fn update_check_item() -> SettingItem {
    SettingItem::new(
        i18n("settings-about-check-for-updates"),
        SettingField::render(|options, _window, cx| {
            Button::new("app-update-check")
                .outline()
                .label(i18n("settings-about-check-button"))
                .disabled(options.is_disabled() || !sparkle::can_check(cx))
                .on_click(|_, _, cx: &mut App| sparkle::check_now(cx))
        }),
    )
}

#[cfg(windows)]
fn update_check_item() -> SettingItem {
    SettingItem::render(move |options, _window, cx| {
        let status = update::status(cx);
        let busy = status.busy();

        let check = Button::new("app-update-check")
            .outline()
            .label(if status == Status::Checking {
                i18n("settings-about-checking")
            } else {
                i18n("settings-about-check-button")
            })
            .disabled(options.is_disabled() || busy)
            .on_click(|_, _, cx: &mut App| update::check_now(cx));

        // The channel resolved a specific release, so the link goes to that
        // one rather than to the list the user would have to find it in. It
        // stays available beside the install button: a package that cannot be
        // installed here can still be downloaded by hand.
        let open = status.release().map(|release| {
            let page_url = release.page_url.clone();
            Button::new("app-update-open")
                .outline()
                .label(i18n("settings-about-open-release"))
                .disabled(options.is_disabled())
                .on_click(move |_, _, cx: &mut App| cx.open_url(&page_url))
        });

        let install = matches!(status, Status::Available(_)).then(|| {
            Button::new("app-update-install")
                .primary()
                .label(i18n("settings-about-install-button"))
                .disabled(options.is_disabled())
                .on_click(|_, window, cx: &mut App| update::install_now(window, cx))
        });

        // The status line reports the result of a check the user just ran and
        // changes while it runs, so it sits under the label instead of behind
        // the hover hint the static row descriptions use: watching a check
        // progress should not require holding the pointer over an icon.
        h_flex()
            .w_full()
            .justify_between()
            .items_center()
            .gap_3()
            .child(
                v_flex()
                    .flex_1()
                    .child(Label::new(i18n("settings-about-check-for-updates")).text_sm())
                    .child(
                        Label::new(status_text(&status))
                            .text_xs()
                            .text_color(cx.theme().muted_foreground),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .children(open)
                    .children(install)
                    .child(check),
            )
            .into_any_element()
    })
}

#[cfg(windows)]
fn status_text(status: &Status) -> String {
    match status {
        Status::Unknown => i18n("settings-about-not-checked").to_string(),
        Status::Checking => i18n("settings-about-checking").to_string(),
        Status::NothingPublished => i18n("settings-about-nothing-published").to_string(),
        Status::UpToDate => i18n("settings-about-up-to-date").replace("{version}", APP_VERSION),
        Status::Available(release) => {
            i18n("settings-about-update-available").replace("{version}", &release.label)
        }
        Status::Installing(release) => {
            i18n("settings-about-installing").replace("{version}", &release.label)
        }
        Status::InspectingFileUse(release) => {
            i18n("settings-about-file-use-checking").replace("{version}", &release.label)
        }
        Status::AwaitingFileUse(release) => {
            i18n("settings-about-file-use-waiting").replace("{version}", &release.label)
        }
        Status::ClosingFileUsers(release) => {
            i18n("settings-about-file-use-closing").replace("{version}", &release.label)
        }
        Status::RecoveryWarning { applications, .. } => {
            i18n("settings-about-recovery-warning-status")
                .replace("{applications}", &applications.join(", "))
        }
        Status::InstallFailed(error) => install_error_text(error),
        Status::Failed(CheckError::Unreachable) => {
            i18n("settings-about-check-unreachable").to_string()
        }
        Status::Failed(CheckError::Unreadable) => {
            i18n("settings-about-check-unreadable").to_string()
        }
    }
}

#[cfg(windows)]
fn install_error_text(error: &InstallError) -> String {
    match error {
        InstallError::NoPackage => i18n("settings-about-install-no-package"),
        InstallError::Unreachable => i18n("settings-about-install-unreachable"),
        InstallError::Checksum => i18n("settings-about-install-checksum"),
        InstallError::Unpack => i18n("settings-about-install-unpack"),
        InstallError::NotWritable => i18n("settings-about-install-not-writable"),
        InstallError::Replace => i18n("settings-about-install-replace"),
        InstallError::Relaunch => i18n("settings-about-install-relaunch"),
    }
    .to_string()
}
