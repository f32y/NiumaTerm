use std::borrow::Cow;

use rust_i18n::t;

use crate::ui::settings::*;

fn agent_hook_item(
    name: Cow<'static, str>,
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
        name.clone(),
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
    let work_and_tool_calls_key: &str = CollapseRows::WorkAndToolCalls.into();
    let tool_calls_key: &str = CollapseRows::ToolCalls.into();
    let off_key: &str = CollapseRows::Off.into();
    let name_and_id_key: &str = ModelListStyle::NameAndId.into();
    let id_and_name_key: &str = ModelListStyle::IdAndName.into();
    let name_only_key: &str = ModelListStyle::NameOnly.into();
    let id_only_key: &str = ModelListStyle::IdOnly.into();

    let installations = agent_updates::installations_for_profiles(agent_profiles, cx);

    let general = SettingGroup::new()
        .title(t!("settings-agent-general"))
        .item(SettingItem::new(
            t!("settings-agent-show-usage"),
            SettingField::switch(
                |cx| cx.global::<AppSettings>().config().agent.show_agent_usage,
                |value, cx| {
                    cx.global_mut::<AppSettings>()
                        .edit_agent(|section| section.show_agent_usage = value);
                },
            ),
        ))
        .item(
            SettingItem::new(
                t!("settings-agent-collapse-tool-calls"),
                SettingField::dropdown(
                    vec![
                        (
                            work_and_tool_calls_key.into(),
                            t!("settings-agent-collapse-work-and-tool-calls").into(),
                        ),
                        (
                            tool_calls_key.into(),
                            t!("settings-agent-collapse-only-tool-calls").into(),
                        ),
                        (off_key.into(), t!("settings-common-off").into()),
                    ],
                    |cx| {
                        let key: &str = cx
                            .global::<AppSettings>()
                            .config()
                            .agent
                            .collapse_tool_calls
                            .into();

                        key.into()
                    },
                    |value, cx| {
                        cx.global_mut::<AppSettings>().edit_agent(|section| {
                            section.collapse_tool_calls = value.as_str().into()
                        });
                    },
                ),
            )
            .description(t!("settings-agent-collapse-tool-calls-description").into_owned()),
        )
        .item(
            SettingItem::new(
                t!("settings-agent-codex-skill-compat"),
                SettingField::switch(
                    |cx| {
                        cx.global::<AppSettings>()
                            .config()
                            .agent
                            .codex_skill_command_compat
                    },
                    |value, cx| {
                        cx.global_mut::<AppSettings>()
                            .edit_agent(|section| section.codex_skill_command_compat = value);
                    },
                ),
            )
            .description(t!("settings-agent-codex-skill-compat-description").into_owned()),
        )
        .item(
            SettingItem::new(
                t!("settings-agent-model-list-style"),
                SettingField::dropdown(
                    vec![
                        (
                            name_and_id_key.into(),
                            t!("settings-agent-model-list-style-name-and-id").into(),
                        ),
                        (
                            id_and_name_key.into(),
                            t!("settings-agent-model-list-style-id-and-name").into(),
                        ),
                        (
                            name_only_key.into(),
                            t!("settings-agent-model-list-style-name-only").into(),
                        ),
                        (
                            id_only_key.into(),
                            t!("settings-agent-model-list-style-id-only").into(),
                        ),
                    ],
                    |cx| {
                        let key: &str = cx
                            .global::<AppSettings>()
                            .config()
                            .agent
                            .model_list_style
                            .into();

                        key.into()
                    },
                    |value, cx| {
                        cx.global_mut::<AppSettings>()
                            .edit_agent(|section| section.model_list_style = value.as_str().into());
                    },
                ),
            )
            .description(t!("settings-agent-model-list-style-description").into_owned()),
        )
        .item(
            SettingItem::new(
                t!("settings-agent-enable-team"),
                SettingField::switch(
                    |cx| cx.global::<AppSettings>().config().agent.enable_agent_team,
                    |value, cx| {
                        cx.global_mut::<AppSettings>()
                            .edit_agent(|section| section.enable_agent_team = value);
                    },
                ),
            )
            .description(t!("settings-agent-enable-team-description").into_owned()),
        );

    let mut cli_updates = SettingGroup::new()
        .title(t!("settings-agent-cli-updates"))
        .item(SettingItem::new(
            t!("settings-agent-check-updates"),
            SettingField::switch(
                |cx| {
                    cx.global::<AppSettings>()
                        .config()
                        .agent
                        .check_agent_updates
                },
                |value, cx| {
                    cx.global_mut::<AppSettings>()
                        .edit_agent(|section| section.check_agent_updates = value);
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

        cli_updates = cli_updates.item(agent_update_status_item(
            index,
            installation_update_title(provider, provider_ordinal, provider_total),
            snapshot.identity.key.clone(),
        ));
    }

    SettingPage::new(t!("settings-agent-title"))
        .default_open(true)
        .group(general)
        .group(
            SettingGroup::new()
                .title(t!("settings-agent-hooks"))
                .item(
                    SettingItem::new(
                        t!("settings-agent-enable-hooks"),
                        SettingField::switch(
                            |cx| cx.global::<AppSettings>().config().agent.enable_agent_hooks,
                            |value, cx| {
                                cx.global_mut::<AppSettings>()
                                    .edit_agent(|section| section.enable_agent_hooks = value);
                            },
                        ),
                    )
                    .description(t!("settings-agent-enable-hooks-description").into_owned()),
                )
                .item(agent_hook_item(
                    t!("settings-agent-kind-claude-code"),
                    claude_hook::settings_path(),
                    claude_hook::settings_path(),
                    claude_hook::hooks_status,
                    claude_hook::install_hooks,
                    claude_hook::uninstall_hooks,
                ))
                .item(agent_hook_item(
                    t!("settings-agent-kind-codex"),
                    codex_hook::config_path(),
                    codex_hook::hooks_path(),
                    codex_hook::hooks_status,
                    codex_hook::install_hooks,
                    codex_hook::uninstall_hooks,
                )),
        )
        .group(cli_updates)
}

pub(super) fn installation_update_title(
    provider: ProviderKind,
    provider_ordinal: usize,
    provider_total: usize,
) -> String {
    if provider_total > 1 {
        t!(
            "settings-agent-updates-title-numbered",
            provider = provider.display(),
            ordinal = provider_ordinal
        )
        .into_owned()
    } else {
        t!(
            "settings-agent-updates-title",
            provider = provider.display()
        )
        .into_owned()
    }
}

pub(super) fn installation_version_text(
    phase: UpdatePhase,
    current: &str,
    available: &str,
) -> String {
    if phase == UpdatePhase::Unknown {
        t!("settings-agent-not-checked").to_string()
    } else {
        format!("{current} → {available}")
    }
}

fn agent_update_check_item() -> SettingItem {
    SettingItem::render(move |options, _window, cx| {
        let profiles = cx
            .global::<AppSettings>()
            .config()
            .agent_profiles
            .list
            .clone();

        let installations = agent_updates::installations_for_profiles(&profiles, cx);
        let busy = installations.iter().any(|snapshot| snapshot.busy);
        let check_profiles = profiles.clone();

        let check = Button::new("agent-updates-check-all")
            .outline()
            .label(if busy {
                t!("settings-agent-working")
            } else {
                t!("settings-agent-check-button")
            })
            .disabled(options.is_disabled() || busy || installations.is_empty())
            .on_click(move |_, _, cx| {
                agent_updates::manual_check_profiles(&check_profiles, cx);
            });

        card_row(t!("settings-agent-check-for-updates"), "", check, cx).into_any_element()
    })
}

fn agent_update_status_item(ix: usize, title: String, key: InstallationKey) -> SettingItem {
    SettingItem::render(move |options, _window, cx| {
        let snapshot = agent_updates::installation(&key, cx);

        let (detail, busy, can_update) = snapshot.map_or_else(
            || {
                (
                    t!("settings-agent-status-unavailable").to_string(),
                    false,
                    false,
                )
            },
            |snapshot| {
                let versions = snapshot.state.versions.as_ref();

                let current = versions
                    .and_then(|status| status.current.as_ref())
                    .map(ToString::to_string)
                    .unwrap_or_else(|| t!("settings-agent-version-unknown").to_string());

                let available = versions
                    .and_then(|status| status.available.as_ref())
                    .map(ToString::to_string)
                    .unwrap_or_else(|| t!("settings-agent-version-unknown").to_string());

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
                        t!(
                            "settings-agent-checked-at",
                            time = time.format("%Y-%m-%d %H:%M")
                        )
                        .into_owned()
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
                    UpdatePhase::Unknown => t!("settings-agent-phase-not-checked"),
                    UpdatePhase::Checking => t!("settings-agent-phase-checking"),
                    UpdatePhase::Current => t!("settings-agent-phase-current"),
                    UpdatePhase::Available => t!("settings-agent-phase-available"),
                    UpdatePhase::WaitingForIdle => t!("settings-agent-phase-waiting-idle"),
                    UpdatePhase::Suspending => t!("settings-agent-phase-suspending"),
                    UpdatePhase::Updating => t!("settings-agent-phase-updating"),
                    UpdatePhase::Verifying => t!("settings-agent-phase-verifying"),
                    UpdatePhase::Restoring => t!("settings-agent-phase-restoring"),
                    UpdatePhase::Updated => t!("settings-agent-phase-updated"),
                    UpdatePhase::Unchanged => t!("settings-agent-phase-unchanged"),
                    UpdatePhase::Unsupported => t!("settings-agent-phase-unsupported"),
                    UpdatePhase::Failed => t!("settings-agent-phase-failed"),
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
            .label(t!("settings-agent-update-button"))
            .disabled(options.is_disabled() || busy || !can_update)
            .on_click(move |_, window, cx| {
                agent_updates::request_update(update_key.clone(), window, cx);
            });

        card_row(title.clone(), detail, update, cx).into_any_element()
    })
}
