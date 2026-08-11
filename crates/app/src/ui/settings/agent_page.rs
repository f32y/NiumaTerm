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
        .title("General")
        .item(
            SettingItem::new(
                "Show Agent Usage",
                SettingField::switch(
                    |cx| cx.global::<AppSettings>().show_agent_usage,
                    |value, cx| {
                        cx.global_mut::<AppSettings>().show_agent_usage = value;
                    },
                ),
            )
            .description("Show Agent account usage in the workspace sidebar."),
        )
        .item(
            SettingItem::new(
                "Collapse Tool Call details by default",
                SettingField::switch(
                    |cx| cx.global::<AppSettings>().collapse_tool_calls,
                    |value, cx| {
                        cx.global_mut::<AppSettings>().collapse_tool_calls = value;
                    },
                ),
            )
            .description(
                "In agent tabs, show only the newest of consecutive tool calls; older \
                 ones sit behind a \"+N previous tool calls\" toggle.",
            ),
        )
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

    SettingPage::new("Agent")
        .default_open(true)
        .description("Configure Agent event handling and per-Agent Hook installation.")
        .group(general)
        .group(
            SettingGroup::new()
                .title("Agent Hooks")
                .item(
                    SettingItem::new(
                        "Enable Agent Hooks",
                        SettingField::switch(
                            |cx| cx.global::<AppSettings>().enable_agent_hooks,
                            |value, cx| {
                                cx.global_mut::<AppSettings>().enable_agent_hooks = value;
                            },
                        ),
                    )
                    .description(
                        "Process new lifecycle events from installed Agent Hooks. This does not change their installation state.",
                    ),
                )
                .item(agent_hook_item(
                    "Claude Code",
                    claude_hook::settings_path(),
                    claude_hook::settings_path(),
                    claude_hook::hooks_status,
                    claude_hook::install_hooks,
                    claude_hook::uninstall_hooks,
                ))
                .item(agent_hook_item(
                    "Codex",
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
        format!("{} Updates {provider_ordinal}", provider.display())
    } else {
        format!("{} Updates", provider.display())
    }
}

pub(super) fn installation_version_text(
    phase: UpdatePhase,
    current: &str,
    available: &str,
) -> String {
    if phase == UpdatePhase::Unknown {
        "Not checked".to_string()
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
            .label(if busy { "Working…" } else { "Check" })
            .disabled(options.disabled || busy || installations.is_empty())
            .on_click(move |_, _, cx| {
                agent_updates::manual_check_profiles(&check_profiles, cx);
            });

        card_row(
            "Check for Updates",
            "Check each distinct Claude Code and Codex installation referenced by Agent Profiles.",
            check,
            cx,
        )
        .into_any_element()
    })
}

fn agent_update_status_item(ix: usize, title: String, key: InstallationKey) -> SettingItem {
    SettingItem::render(move |options, _window, cx| {
        let snapshot = agent_updates::installation(&key, cx);
        let (detail, busy, can_update) = snapshot.map_or_else(
            || ("Update status unavailable".to_string(), false, false),
            |snapshot| {
                let versions = snapshot.state.versions.as_ref();
                let current = versions
                    .and_then(|status| status.current.as_ref())
                    .map(ToString::to_string)
                    .unwrap_or_else(|| "unknown".to_string());
                let available = versions
                    .and_then(|status| status.available.as_ref())
                    .map(ToString::to_string)
                    .unwrap_or_else(|| "unknown".to_string());
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
                    .map(|time| format!(" · checked {}", time.format("%Y-%m-%d %H:%M")))
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
                    UpdatePhase::Unknown => "not checked",
                    UpdatePhase::Checking => "checking",
                    UpdatePhase::Current => "current",
                    UpdatePhase::Available => "update available",
                    UpdatePhase::WaitingForIdle => "waiting for idle",
                    UpdatePhase::Suspending => "stopping agents",
                    UpdatePhase::Updating => "updating",
                    UpdatePhase::Verifying => "verifying",
                    UpdatePhase::Restoring => "restoring tabs",
                    UpdatePhase::Updated => "updated",
                    UpdatePhase::Unchanged => "version unchanged",
                    UpdatePhase::Unsupported => "automatic discovery unsupported",
                    UpdatePhase::Failed => "failed",
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
            .label("Update")
            .disabled(options.disabled || busy || !can_update)
            .on_click(move |_, window, cx| {
                agent_updates::request_update(update_key.clone(), window, cx);
            });

        card_row(title.clone(), detail, update, cx).into_any_element()
    })
}
