use nmt_i18n::i18n;

use crate::ui::settings::*;

fn agent_hook_item(
    name: &'static str,
    detection_path: Option<path::PathBuf>,
    hooks_path: Option<path::PathBuf>,
    status: fn(&path::Path) -> HookInstallStatus,
    install: fn(&path::Path) -> io::Result<()>,
    uninstall: fn(&path::Path) -> io::Result<()>,
) -> SettingItem {
    let detected = detection_path.as_ref().is_some_and(|path| path.is_file());
    let status_path = hooks_path.clone();
    let action_path = hooks_path;

    SettingItem::new(
        name,
        SettingField::checkbox(
            // Settings renders only the active page, so a disk-backed getter
            // refreshes Hook state whenever the user enters the Agent page.
            move |_| {
                status_path
                    .as_deref()
                    .is_some_and(|path| status(path) == HookInstallStatus::Installed)
            },
            move |enabled, cx| {
                let Some(path) = action_path.as_deref() else {
                    return;
                };

                let result = if enabled {
                    install(path)
                } else {
                    uninstall(path)
                };

                if let Err(error) = result {
                    warn!("failed to update {name} hooks: {error}");
                }

                cx.refresh_windows();
            },
        ),
    )
    .disabled(!detected)
}

pub(super) fn agent_page(agent_profiles: &[AgentProfile], cx: &App) -> SettingPage {
    let installations = agent_updates::installations_for_profiles(agent_profiles, cx);
    let mut general = SettingGroup::new()
        .title(i18n("settings-agent-general"))
        .item(SettingItem::new(
            i18n("settings-agent-show-usage"),
            SettingField::switch(
                |cx| cx.global::<AppSettings>().show_agent_usage,
                |value, cx| {
                    cx.global_mut::<AppSettings>().show_agent_usage = value;
                },
            ),
        ))
        .item(SettingItem::new(
            i18n("settings-agent-collapse-tool-calls"),
            SettingField::switch(
                |cx| cx.global::<AppSettings>().collapse_tool_calls,
                |value, cx| {
                    cx.global_mut::<AppSettings>().collapse_tool_calls = value;
                },
            ),
        ))
        .item(SettingItem::new(
            i18n("settings-agent-check-updates"),
            SettingField::switch(
                |cx| cx.global::<AppSettings>().check_agent_updates,
                |value, cx| {
                    cx.global_mut::<AppSettings>().check_agent_updates = value;
                },
            ),
        ))
        .item(agent_update_check_item());

    for (index, snapshot) in installations.iter().enumerate() {
        let provider = snapshot.identity.provider;
        let provider_total = installations
            .iter()
            .filter(|item| item.identity.provider == provider)
            .count();
        let provider_ordinal = installations[..=index]
            .iter()
            .filter(|item| item.identity.provider == provider)
            .count();
        general = general.item(agent_update_status_item(
            index,
            installation_update_title(provider, provider_ordinal, provider_total),
            snapshot.identity.key.clone(),
        ));
    }

    SettingPage::new(i18n("settings-agent-title"))
        .default_open(true)
        .group(general)
        .group(
            SettingGroup::new()
                .title(i18n("settings-agent-hooks"))
                .item(
                    SettingItem::new(
                        i18n("settings-agent-enable-hooks"),
                        SettingField::switch(
                            |cx| cx.global::<AppSettings>().enable_agent_hooks,
                            |value, cx| {
                                cx.global_mut::<AppSettings>().enable_agent_hooks = value;
                            },
                        ),
                    )
                    .description(i18n("settings-agent-enable-hooks-description")),
                )
                .item(agent_hook_item(
                    i18n("settings-agent-kind-claude-code"),
                    claude_hook::settings_path(),
                    claude_hook::settings_path(),
                    claude_hook::hooks_status,
                    claude_hook::install_hooks,
                    claude_hook::uninstall_hooks,
                ))
                .item(agent_hook_item(
                    i18n("settings-agent-kind-codex"),
                    codex_hook::config_path(),
                    codex_hook::hooks_path(),
                    codex_hook::hooks_status,
                    codex_hook::install_hooks,
                    codex_hook::uninstall_hooks,
                )),
        )
}

pub(super) fn installation_update_title(
    provider: ProviderKind,
    provider_ordinal: usize,
    provider_total: usize,
) -> String {
    if provider_total > 1 {
        i18n("settings-agent-updates-title-numbered")
            .replace("{provider}", provider.display())
            .replace("{ordinal}", &provider_ordinal.to_string())
    } else {
        i18n("settings-agent-updates-title").replace("{provider}", provider.display())
    }
}

pub(super) fn installation_version_text(
    phase: UpdatePhase,
    current: &str,
    available: &str,
) -> String {
    if phase == UpdatePhase::Unknown {
        i18n("settings-agent-not-checked").to_string()
    } else {
        format!("{current} → {available}")
    }
}

fn agent_update_check_item() -> SettingItem {
    SettingItem::render(move |options, _window, cx| {
        let profiles = cx.global::<AppSettings>().agent_profiles.clone();
        let installations = agent_updates::installations_for_profiles(&profiles, cx);
        let busy = installations.iter().any(|snapshot| snapshot.busy);
        let check_profiles = profiles.clone();
        let check = Button::new("agent-updates-check-all")
            .outline()
            .label(if busy {
                i18n("settings-agent-working")
            } else {
                i18n("settings-agent-check-button")
            })
            .disabled(options.disabled || busy || installations.is_empty())
            .on_click(move |_, _, cx| {
                agent_updates::manual_check_profiles(&check_profiles, cx);
            });

        card_row(i18n("settings-agent-check-for-updates"), "", check, cx).into_any_element()
    })
}

fn agent_update_status_item(ix: usize, title: String, key: InstallationKey) -> SettingItem {
    SettingItem::render(move |options, _window, cx| {
        let snapshot = agent_updates::installation(&key, cx);
        let (detail, busy, can_update) = snapshot.map_or_else(
            || {
                (
                    i18n("settings-agent-status-unavailable").to_string(),
                    false,
                    false,
                )
            },
            |snapshot| {
                let versions = snapshot.state.versions.as_ref();
                let current = versions
                    .and_then(|status| status.current.as_ref())
                    .map(ToString::to_string)
                    .unwrap_or_else(|| i18n("settings-agent-version-unknown").to_string());
                let available = versions
                    .and_then(|status| status.available.as_ref())
                    .map(ToString::to_string)
                    .unwrap_or_else(|| i18n("settings-agent-version-unknown").to_string());
                let labels = versions
                    .map(|status| {
                        [status.install_method.as_deref(), status.channel.as_deref()]
                            .into_iter()
                            .flatten()
                            .collect::<Vec<_>>()
                            .join(" · ")
                    })
                    .filter(|labels| !labels.is_empty())
                    .map(|labels| format!(" · {labels}"))
                    .unwrap_or_default();
                let checked = snapshot
                    .last_checked
                    .map(|time| {
                        i18n("settings-agent-checked-at")
                            .replace("{time}", &time.format("%Y-%m-%d %H:%M").to_string())
                    })
                    .unwrap_or_default();
                let diagnostic = snapshot
                    .state
                    .error
                    .as_ref()
                    .map(|error| error.message())
                    .or_else(|| {
                        versions.and_then(|status| match &status.support {
                            DiscoverySupport::Supported => None,
                            DiscoverySupport::Unsupported { reason } => Some(reason.as_str()),
                        })
                    })
                    .map(|message| format!(" · {}", message.chars().take(256).collect::<String>()))
                    .unwrap_or_default();
                let phase = match snapshot.state.phase {
                    UpdatePhase::Unknown => i18n("settings-agent-phase-not-checked"),
                    UpdatePhase::Checking => i18n("settings-agent-phase-checking"),
                    UpdatePhase::Current => i18n("settings-agent-phase-current"),
                    UpdatePhase::Available => i18n("settings-agent-phase-available"),
                    UpdatePhase::WaitingForIdle => i18n("settings-agent-phase-waiting-idle"),
                    UpdatePhase::Suspending => i18n("settings-agent-phase-suspending"),
                    UpdatePhase::Updating => i18n("settings-agent-phase-updating"),
                    UpdatePhase::Verifying => i18n("settings-agent-phase-verifying"),
                    UpdatePhase::Restoring => i18n("settings-agent-phase-restoring"),
                    UpdatePhase::Updated => i18n("settings-agent-phase-updated"),
                    UpdatePhase::Unchanged => i18n("settings-agent-phase-unchanged"),
                    UpdatePhase::Unsupported => i18n("settings-agent-phase-unsupported"),
                    UpdatePhase::Failed => i18n("settings-agent-phase-failed"),
                };
                let can_update =
                    versions.is_some_and(|status| status.can_update && status.update_available());
                let version = installation_version_text(snapshot.state.phase, &current, &available);
                let detail = if snapshot.state.phase == UpdatePhase::Unknown {
                    version
                } else {
                    format!("{version} · {phase}{labels}{checked}{diagnostic}")
                };
                (detail, snapshot.busy, can_update)
            },
        );

        let update_key = key.clone();
        let update = Button::new(("agent-update-install", ix))
            .primary()
            .label(i18n("settings-agent-update-button"))
            .disabled(options.disabled || busy || !can_update)
            .on_click(move |_, window, cx| {
                agent_updates::request_update(update_key.clone(), window, cx);
            });

        card_row(title.clone(), detail, update, cx).into_any_element()
    })
}
